#!/usr/bin/env python3
"""Generation-local release contract, acquisition and update-check policy.

No command in this module installs a generation or changes a profile. The shell
transaction owns publication of the host and guest selection. All sizes are byte
counts; final artifacts are immutable SHA-256 addresses, under owner-only locks.
The parser is shared by explicit checks, automatic checks and installation.
"""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

MANIFEST_LIMIT = 256 * 1024
HOST_LIMIT = 128 * 1024 * 1024
GUEST_LIMIT = 2 * 1024 * 1024 * 1024 - 1
VIRTUAL_SIZE = 8 * 1024 * 1024 * 1024
MAX_COUNTER = (1 << 64) - 1


def stable_version(value):
    if not isinstance(value, str) or not re.fullmatch(r"v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
        raise ValueError("version must be canonical stable X.Y.Z")
    parts = tuple(int(part) for part in value.removeprefix("v").split("."))
    if any(part > 0xffffffff for part in parts):
        raise ValueError("version component overflow")
    return parts


def pairs(values):
    result = {}
    for key, value in values:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def decode(data):
    try:
        return json.loads(data, object_pairs_hook=pairs,
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError("invalid JSON number")))
    except RecursionError:
        raise ValueError("JSON nesting exceeds the supported depth") from None


def keys(value, expected, label):
    if not isinstance(value, dict) or set(value) != set(expected):
        raise ValueError(label + " has an invalid schema")


def integer(value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise ValueError("integer outside permitted range")
    return value


def safe_file(path, private=False, limit=None):
    path = Path(path)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1:
        raise ValueError("unsafe owned regular file")
    if info.st_mode & 0o022 or (private and stat.S_IMODE(info.st_mode) != 0o600):
        raise ValueError("unsafe file permissions")
    if limit is not None and info.st_size > limit:
        raise ValueError("file exceeds size limit")
    return info


def read_file(path, limit, private=False):
    info = safe_file(path, private, limit)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as source:
        opened = os.fstat(source.fileno())
        if (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino):
            raise ValueError("file changed while opening")
        data = source.read(limit + 1)
    if len(data) > limit:
        raise ValueError("file exceeds size limit")
    return data


def directory(path, mode=0o700):
    path = Path(path)
    try:
        path.mkdir(mode=mode)
    except FileExistsError:
        pass
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise ValueError("unsafe cache directory")
    if mode == 0o700 and stat.S_IMODE(info.st_mode) != mode:
        raise ValueError("cache directory must be private")
    return path


def cache_root(home):
    root = directory(Path(home) / ".hamn")
    return directory(root / "cache", 0o755)


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic_json(path, value):
    path = Path(path)
    if path.exists() or path.is_symlink():
        safe_file(path, private=True)
    fd, temporary = tempfile.mkstemp(prefix="." + path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as output:
            json.dump(value, output, sort_keys=True, separators=(",", ":"))
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


@contextlib.contextmanager
def lock(path, blocking=True):
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or stat.S_IMODE(info.st_mode) != 0o600:
            raise ValueError("unsafe cache lock")
        fcntl.flock(fd, fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
        yield
    finally:
        os.close(fd)


def local_source(url):
    if os.environ.get("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS") != "1":
        return None
    if url.startswith("file://"):
        return url[7:]
    return url if url.startswith("/") else None


def validate_url(url):
    if not isinstance(url, str) or not url or any(ord(c) < 33 or ord(c) > 126 for c in url):
        raise ValueError("artifact URL is invalid")
    if local_source(url) is not None:
        return
    if url.startswith(("file://", "/")):
        raise ValueError("local artifacts are disabled")
    parsed = urlsplit(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
        raise ValueError("artifact URL must use HTTPS")


def parse_manifest(data, macos, architecture):
    if len(data) > MANIFEST_LIMIT:
        raise ValueError("manifest exceeds 256 KiB")
    value = decode(data)
    if isinstance(value, dict) and value.get("schemaVersion") == 2 and "repository" in value:
        repository = value.pop("repository")
        if not isinstance(repository, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
            raise ValueError("release repository is invalid")
    keys(value, ("schemaVersion", "channel", "version", "commit", "validationMode", "compatibility", "artifacts"), "manifest")
    if type(value["schemaVersion"]) is not int or value["schemaVersion"] not in (2, 3) or value["channel"] != "stable":
        raise ValueError("manifest is not a stable schema v2 or v3 release")
    stable_version(value["version"])
    value["version"] = "v" + value["version"].removeprefix("v")
    if not isinstance(value["commit"], str) or not re.fullmatch(r"[0-9a-f]{40}", value["commit"]):
        raise ValueError("release commit is invalid")
    if value["validationMode"] not in ("github-hosted-no-vm", "physical-apple-silicon"):
        raise ValueError("release validation mode is invalid")
    compatibility = value["compatibility"]
    keys(compatibility, ("os", "architecture", "minimumMacOS"), "compatibility")
    def system_version(text):
        if not isinstance(text, str) or not re.fullmatch(r"[0-9]+(?:\.[0-9]+){0,2}", text):
            raise ValueError("invalid macOS version")
        parts = tuple(int(part) for part in text.split("."))
        return parts + (0,) * (3 - len(parts))
    if compatibility["os"] != "darwin" or compatibility["architecture"] != "arm64" or architecture not in ("arm64", "arm64e"):
        raise ValueError("manifest is not compatible with Apple Silicon macOS")
    if system_version(macos) < system_version(compatibility["minimumMacOS"]):
        raise ValueError("macOS is below the release minimum")
    keys(value["artifacts"], ("host", "guestImage"), "artifacts")
    for name, artifact in value["artifacts"].items():
        expected = ["url", "sha256"]
        if value["schemaVersion"] == 3:
            expected += ["size"]
            if name == "guestImage":
                expected += ["format", "compression", "virtualSize"]
        keys(artifact, expected, name)
        validate_url(artifact["url"])
        if not isinstance(artifact["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"]):
            raise ValueError("artifact SHA-256 is invalid")
        if value["schemaVersion"] == 3:
            integer(artifact["size"], 1, HOST_LIMIT if name == "host" else GUEST_LIMIT)
            if name == "guestImage" and (artifact["format"] != "qcow2" or artifact["compression"] != "zlib" or type(artifact["virtualSize"]) is not int or artifact["virtualSize"] != VIRTUAL_SIZE):
                raise ValueError("unsupported guest image format")
    return value


def digest(path):
    info = safe_file(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as source:
        actual = os.fstat(source.fileno())
        if (actual.st_dev, actual.st_ino) != (info.st_dev, info.st_ino):
            raise ValueError("artifact changed while opening")
        result = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def verified(path, artifact, limit):
    try:
        info = safe_file(path, limit=limit)
        return (artifact.get("size") is None or info.st_size == artifact["size"]) and digest(path) == artifact["sha256"]
    except FileNotFoundError:
        return False


def response_headers(path):
    headers = {}
    status = 0
    for line in Path(path).read_text(encoding="latin1").splitlines():
        if line.startswith("HTTP/"):
            headers = {}
            status = int(line.split()[1])
        elif ":" in line:
            name, value = line.split(":", 1)
            headers[name.lower()] = value.strip()
    return status, headers


class RangeRejected(ValueError):
    def __init__(self, message, received=0):
        super().__init__(message)
        self.received = received


def transfer(url, destination, limit, offset=0, validator=None, automatic=False, on_headers=None):
    """Append a verified Range response; bound bytes while reading curl stdout.

    curl owns TLS/redirect checks and wall-clock timeout. Every child is reaped
    on all exits. Partial files survive only network interruption, never an
    integrity/size/Range failure. Return actual HTTP body bytes and validator.
    """
    validate_url(url)
    local = local_source(url)
    if local is not None:
        if offset:
            raise RangeRejected("local artifacts do not support Range")
        info = safe_file(local, limit=limit)
        shutil.copyfile(local, destination)
        os.chmod(destination, 0o600)
        return 0, None, info.st_size
    header_fd, header_path = tempfile.mkstemp(prefix=".http-", dir=Path(destination).parent)
    os.close(header_fd)
    command = ["curl", "--fail", "--show-error", "--silent", "--location", "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2",
               "--connect-timeout", "2" if automatic else "15", "--max-time", "5" if automatic else "600",
               "--dump-header", header_path, "-o", "-"]
    if offset:
        command += ["--range", str(offset) + "-"]
        if validator:
            command += ["--header", "If-Range: " + validator]
    command.append(url)
    child = None
    received = 0
    checked = False
    range_rejected = False
    try:
        fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | (os.O_APPEND if offset else os.O_TRUNC), 0o600)
        with os.fdopen(fd, "wb") as target:
            safe_file(destination, private=True)
            child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            while True:
                block = child.stdout.read(64 * 1024)
                if not block:
                    break
                if not checked:
                    status, headers = response_headers(header_path)
                    if offset:
                        match = re.fullmatch(r"bytes ([0-9]+)-([0-9]+)/([0-9]+)", headers.get("content-range", ""))
                        if status != 206 or not match or int(match[1]) != offset or int(match[3]) != limit or int(match[2]) != limit - 1:
                            range_rejected = True
                    if on_headers and not range_rejected:
                        on_headers(headers)
                    checked = True
                received += len(block)
                if range_rejected:
                    if received > limit:
                        raise ValueError("ignored Range response exceeds size limit")
                    continue
                if offset + received > limit:
                    raise ValueError("artifact exceeds expected size")
                target.write(block)
            target.flush()
            os.fsync(target.fileno())
        rc = child.wait()
        status, headers = response_headers(header_path)
        if offset and (range_rejected or status in (200, 416) or (rc == 0 and not checked)):
            raise RangeRejected("server rejected Range", received)
        if rc != 0:
            raise OSError("release transfer failed")
        validator = headers.get("etag") or headers.get("last-modified")
        if validator and (len(validator) > 1024 or any(ord(c) < 32 or ord(c) > 126 for c in validator)):
            validator = None
        return received, validator, received
    finally:
        if child is not None:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            if child.stdout:
                child.stdout.close()
        os.unlink(header_path)


def fetch_manifest(url, macos, architecture, automatic=False):
    with tempfile.TemporaryDirectory(prefix="hamn-manifest-") as temporary:
        path = Path(temporary) / "manifest.json"
        count, _, _ = transfer(url, path, MANIFEST_LIMIT, automatic=automatic)
        return parse_manifest(read_file(path, MANIFEST_LIMIT), macos, architecture), count


def acquire(cache, artifact, name):
    limit = HOST_LIMIT if name == "host" else GUEST_LIMIT
    downloads = directory(Path(cache) / "downloads")
    key = artifact["sha256"]
    final = downloads / (key + ".artifact")
    partial = downloads / ("." + key + ".partial")
    metadata = downloads / ("." + key + ".validator")
    counts = {"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0, "source": "download"}
    with lock(downloads / ("." + key + ".lock")):
        if verified(final, artifact, limit):
            counts.update(reusedBytes=final.stat().st_size, source="cache")
            return final, counts
        if final.exists() or final.is_symlink():
            safe_file(final, private=True, limit=limit)
            final.unlink()  # Only our verified type/ownership; damaged content is never reused.
        guest = Path(cache) / ("hamn-guest-" + key + ".img")
        if name == "guestImage" and verified(guest, artifact, limit):
            counts.update(reusedBytes=guest.stat().st_size, source="guest-cache")
            return guest, counts
        expected = artifact.get("size")
        offset = 0
        validator = None
        if partial.exists() or partial.is_symlink():
            info = safe_file(partial, private=True, limit=limit)
            if expected is not None and info.st_size == expected and verified(partial, artifact, limit):
                # A crash/rename failure after the final fsync must not turn a
                # complete verified payload into another network download.
                if metadata.exists() or metadata.is_symlink():
                    safe_file(metadata, private=True, limit=4096)
                os.replace(partial, final)
                sync_directory(downloads)
                metadata.unlink(missing_ok=True)
                counts.update(reusedBytes=expected, source="partial-cache")
                return final, counts
            try:
                saved = decode(read_file(metadata, 4096, private=True))
                keys(saved, ("schemaVersion", "sha256", "size", "validator"), "partial metadata")
                if expected is not None and saved["schemaVersion"] == 1 and saved["sha256"] == key and saved["size"] == expected and 0 < info.st_size < expected:
                    offset = info.st_size
                    validator = saved["validator"]
                    if validator is not None and (not isinstance(validator, str) or len(validator) > 1024 or any(ord(c) < 32 or ord(c) > 126 for c in validator)):
                        offset = 0
            except (OSError, ValueError, TypeError):
                offset = 0
            if not offset:
                partial.unlink()
        if expected is not None:
            atomic_json(metadata, {"schemaVersion": 1, "sha256": key, "size": expected, "validator": validator})
        def save_validator(headers):
            value = headers.get("etag") or headers.get("last-modified")
            if expected is not None and value and len(value) <= 1024 and all(32 <= ord(c) <= 126 for c in value):
                atomic_json(metadata, {"schemaVersion": 1, "sha256": key, "size": expected, "validator": value})
        discarded = 0
        try:
            try:
                received, validator, written = transfer(artifact["url"], partial, expected or limit, offset, validator, on_headers=save_validator)
            except RangeRejected as rejected:
                discarded = rejected.received
                partial.unlink(missing_ok=True)
                offset = 0
                received, validator, written = transfer(artifact["url"], partial, expected or limit, on_headers=save_validator)
            if not verified(partial, artifact, limit):
                raise ValueError("artifact size or SHA-256 mismatch")
            os.replace(partial, final)
            sync_directory(downloads)
            metadata.unlink(missing_ok=True)
            counts.update(downloadedBytes=received + discarded, resumedBytes=received if offset else 0,
                          reusedBytes=offset if received else written,
                          source="resumed" if offset else ("local" if received == 0 else "download"))
            return final, counts
        except ValueError:
            partial.unlink(missing_ok=True)
            metadata.unlink(missing_ok=True)
            raise
        except OSError:
            if expected is None:
                partial.unlink(missing_ok=True)
            raise


def guest_healthy(cache, artifact):
    cache = Path(cache)
    key = artifact["sha256"]
    name = "hamn-guest-" + key + ".img"
    try:
        selection = decode(read_file(cache / "guest-image.json", 4096))
        marker = read_file(cache / (name + ".verified"), 128).decode().strip()
        return selection == {"schemaVersion": 1, "file": name, "sha256": key} and marker == key and verified(cache / name, artifact, GUEST_LIMIT)
    except (OSError, ValueError, TypeError):
        return False


def receipt(mode, target, manifest, cache):
    generation = Path(target).parent.parent
    path = generation / ".hamn-release.json"
    identity = {"schemaVersion": 1, "version": manifest["version"],
                "hostSHA256": manifest["artifacts"]["host"]["sha256"],
                "guestSHA256": manifest["artifacts"]["guestImage"]["sha256"]}
    entries = []
    def visit(path, name):
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            if info.st_uid != os.getuid() or info.st_mode & 0o022:
                raise ValueError("unsafe installed directory")
            entries.append((name, stat.S_IMODE(info.st_mode), None))
            for child in sorted(path.iterdir()):
                visit(child, name + "/" + child.name)
        else:
            entries.append((name, stat.S_IMODE(info.st_mode), digest(path)))
    for name, child in (("bin", generation / "bin"), ("scripts", generation / "share/hamn/src/scripts"), ("packaging", generation / "share/hamn/src/packaging")):
        visit(child, name)
    identity["installedSHA256"] = hashlib.sha256(json.dumps(entries, separators=(",", ":")).encode()).hexdigest()
    if mode == "write":
        # A generation receives one receipt. Never overwrite prior evidence.
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "w") as output:
            json.dump(identity, output, sort_keys=True, separators=(",", ":"))
            output.write("\n"); output.flush(); os.fsync(output.fileno())
        sync_directory(generation)
        return True
    if decode(read_file(path, 4096, private=True)) != identity:
        return False
    return mode == "host-check" or guest_healthy(cache, manifest["artifacts"]["guestImage"])


def host_healthy(target, manifest, cache):
    try:
        return bool(target) and receipt("host-check", target, manifest, cache)
    except (OSError, ValueError, TypeError):
        return False


def version_status(current, manifest, cache, target=None):
    comparison = (stable_version(manifest["version"]) > stable_version(current)) - (stable_version(manifest["version"]) < stable_version(current))
    healthy = not os.path.lexists(Path(cache) / ".hamn-update-transaction") and guest_healthy(cache, manifest["artifacts"]["guestImage"]) and host_healthy(target, manifest, cache)
    return "update-available" if comparison > 0 else "ahead" if comparison < 0 else "up-to-date" if healthy else "repair-required"


def result(current, manifest, status, counts):
    if set(counts) - {"manifest", "host", "guestImage"}:
        raise ValueError("unknown transfer accounting source")
    counts = {name: counts.get(name, {"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0, "source": "none"}) for name in ("manifest", "host", "guestImage")}
    total = {}
    for field in ("downloadedBytes", "resumedBytes", "reusedBytes"):
        total[field] = sum(integer(item.get(field, 0), 0, MAX_COUNTER) for item in counts.values())
        integer(total[field], 0, MAX_COUNTER)
    return {"schemaVersion": 1, "currentVersion": current.removeprefix("v"), "latestVersion": manifest["version"].removeprefix("v"),
            "status": status, **total, "artifacts": counts, "profileDisksChanged": False, "completed": True}


def unsupported_result(current):
    return {"schemaVersion": 1, "currentVersion": current, "latestVersion": None,
            "status": "unsupported-install", "downloadedBytes": 0,
            "resumedBytes": 0, "reusedBytes": 0, "artifacts": {
                name: {"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0, "source": "none"}
                for name in ("manifest", "host", "guestImage")},
            "profileDisksChanged": False, "completed": True}


def automatic(args):
    """Detached process only; never called in foreground. No profile data read."""
    stable_version(args.current_version)
    cache = cache_root(args.home)
    now = int(time.time())
    with lock(cache / ".update-check.lock", blocking=False):
        existing = read_check(cache, now)
        if existing and now - existing["checkedAt"] < (86400 if existing["ok"] else 21600):
            return
        record = {"schemaVersion": 1, "checkedAt": now, "ok": False, "latestVersion": None}
        try:
            manifest, _ = fetch_manifest(args.manifest, args.macos, args.architecture, automatic=True)
            record.update(ok=True, latestVersion=manifest["version"].removeprefix("v"))
        except (OSError, ValueError, TypeError):
            if existing and existing["ok"]:
                record["latestVersion"] = existing["latestVersion"]
        atomic_json(cache / "update-check-v1.json", record)


def read_check(cache, now):
    try:
        record = decode(read_file(Path(cache) / "update-check-v1.json", 4096, private=True))
        keys(record, ("schemaVersion", "checkedAt", "ok", "latestVersion"), "update check cache")
        if type(record["schemaVersion"]) is not int or record["schemaVersion"] != 1 or type(record["ok"]) is not bool or (record["ok"] and record["latestVersion"] is None):
            return None
        integer(record["checkedAt"], 0, now)
        if record["latestVersion"] is not None:
            stable_version(record["latestVersion"])
        return record
    except (OSError, ValueError, TypeError):
        return None


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    command = commands.add_parser("version")
    command.add_argument("current")
    for name in ("manifest", "check", "automatic", "schedule"):
        command = commands.add_parser(name)
        command.add_argument("--manifest", required=True)
        command.add_argument("--macos", required=name not in ("automatic", "schedule"))
        command.add_argument("--architecture", required=name not in ("automatic", "schedule"))
        command.add_argument("--home", default=os.environ.get("HOME", ""))
        command.add_argument("--current-version", default="0.0.0")
        command.add_argument("--output")
        command.add_argument("--target")
    command = commands.add_parser("fields")
    command.add_argument("manifest")
    command = commands.add_parser("acquire")
    command.add_argument("manifest")
    command.add_argument("name", choices=("host", "guestImage"))
    command.add_argument("cache")
    command.add_argument("counts")
    command = commands.add_parser("receipt")
    command.add_argument("manifest")
    command.add_argument("mode", choices=("check", "host-check", "write"))
    command.add_argument("target")
    command.add_argument("cache")
    command = commands.add_parser("status")
    command.add_argument("manifest")
    command.add_argument("current")
    command.add_argument("cache")
    command.add_argument("target", nargs="?")
    command = commands.add_parser("result")
    command.add_argument("manifest")
    command.add_argument("current")
    command.add_argument("status")
    command.add_argument("counts")
    command = commands.add_parser("reuse-counts")
    command.add_argument("manifest")
    command.add_argument("cache")
    command.add_argument("counts")
    command.add_argument("name", choices=("host", "guestImage", "both"))
    args = parser.parse_args()
    if args.command == "version":
        stable_version(args.current)
        return
    if args.command == "schedule":
        # The short-lived launcher is reaped by Rust. Double-fork makes the
        # network worker an orphan adopted by the OS reaper, even when the
        # calling TUI remains alive after this function returns.
        first = os.fork()
        if first:
            os.waitpid(first, 0)
            return
        os.setsid()
        second = os.fork()
        if second:
            os._exit(0)
        signal.signal(signal.SIGHUP, signal.SIG_IGN)
        args.command = "automatic"
    if args.command == "automatic":
        args.macos = args.macos or subprocess.check_output(["/usr/bin/sw_vers", "-productVersion"], text=True).strip()
        args.architecture = args.architecture or os.uname().machine
        automatic(args)
    elif args.command in ("manifest", "check"):
        try:
            stable_version(args.current_version)
        except ValueError:
            if args.command == "check":
                print(json.dumps(unsupported_result(args.current_version), sort_keys=True))
                return
            raise
        manifest, count = fetch_manifest(args.manifest, args.macos, args.architecture)
        if args.command == "manifest":
            atomic_json(args.output, manifest)
            print(count)
        else:
            status = version_status(args.current_version, manifest, Path(args.home) / ".hamn/cache", args.target)
            print(json.dumps(result(args.current_version, manifest, status, {"manifest": {"downloadedBytes": count, "resumedBytes": 0, "reusedBytes": 0, "source": "network" if count else "local"}}), sort_keys=True))
    else:
        manifest = decode(read_file(args.manifest, MANIFEST_LIMIT))
        if args.command == "fields":
            print(manifest["version"])
            for name in ("host", "guestImage"):
                print(manifest["artifacts"][name]["url"])
                print(manifest["artifacts"][name]["sha256"])
        elif args.command == "status":
            print(version_status(args.current, manifest, args.cache, args.target))
        elif args.command == "receipt":
            if not receipt(args.mode, args.target, manifest, args.cache):
                sys.exit(1)
        elif args.command == "acquire":
            path, counts = acquire(args.cache, manifest["artifacts"][args.name], args.name)
            atomic_json(args.counts, counts)
            print(path)
        elif args.command == "result":
            counts = {}
            for path in Path(args.counts).glob("*.json"):
                counts[path.stem] = decode(read_file(path, 4096, private=True))
            print(json.dumps(result(args.current, manifest, args.status, counts), sort_keys=True))
        elif args.command == "reuse-counts":
            for name in ("host", "guestImage") if args.name == "both" else (args.name,):
                artifact = manifest["artifacts"][name]
                amount = artifact.get("size", 0)
                if not amount:
                    path = Path(args.cache) / ("hamn-guest-" + artifact["sha256"] + ".img") if name == "guestImage" else Path(args.cache) / "downloads" / (artifact["sha256"] + ".artifact")
                    if path.exists():
                        amount = safe_file(path).st_size
                atomic_json(Path(args.counts) / (name + ".json"), {"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": amount, "source": "installed"})


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError) as error:
        message = "release metadata, cache, or transfer validation failed"
        if isinstance(error, ValueError):
            message = "".join(c if 32 <= ord(c) < 127 else "?" for c in str(error))[:180]
        print("hamn upgrade: " + message, file=sys.stderr)
        sys.exit(1)
