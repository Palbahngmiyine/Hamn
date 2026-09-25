#!/usr/bin/env python3
"""Bounded generated upgrade cases, seed 20260921, against the shipped binary.

These complement the fixed fault/signal matrices and the native Rust property
tests in control/install_support; they are not a proof over all inputs or a
guest boot/functional-equivalence test. All installs, network observations,
processes and files belong to a temporary fixture. No build output is changed.
"""
import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import random
import selectors
import shlex
import shutil
import signal
import socketserver
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import patch

from test_upgrade_native import BINARY, ROOT, manifest

SEED = 20260921


@contextlib.contextmanager
def network_tripwire(root):
    """Observe any attempted TLS connection, independently of curl's path."""
    requests = root / "unexpected-request"

    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            requests.write_text("unexpected network connection")

    with socketserver.TCPServer(("127.0.0.1", 0), Handler) as server:
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            yield server.server_address[1], requests
        finally:
            server.shutdown()
            worker.join(timeout=2)
            assert not worker.is_alive(), "network observation thread survived"

# Exact parser from scripts/update-host.sh at ed7a7023c073bb3cc3dc248f4acd1583424ce9e3
# (source blob b38ff62947c5b7a1362150c8723fc75ddb9870a3). Freeze the old consumer
# so changes to today's parser cannot silently redefine legacy compatibility.
LEGACY_PARSER = r'''import json
import re
import sys


def pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key: " + key)
        result[key] = value
    return result


def version(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9]+(?:\.[0-9]+){0,2}", value):
        raise ValueError("invalid macOS version")
    return tuple(int(part) for part in value.split("."))


def require_keys(value, keys, label):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ValueError(label + " has an invalid schema")


def artifact(value, label):
    require_keys(value, ("url", "sha256"), label)
    url = value["url"]
    digest = value["sha256"]
    if not isinstance(url, str) or not url or any(ord(ch) < 33 or ord(ch) > 126 for ch in url):
        raise ValueError(label + " URL is invalid")
    if not re.fullmatch(r"[0-9a-f]{64}", digest if isinstance(digest, str) else ""):
        raise ValueError(label + " SHA-256 is invalid")
    return url, digest


try:
    with open(sys.argv[1], encoding="utf-8") as source:
        manifest = json.load(source, object_pairs_hook=pairs,
                             parse_constant=lambda value: (_ for _ in ()).throw(ValueError(value)))
    # v0.1.x published repository as optional descriptive metadata. Accept
    # that exact extension, while retaining strict rejection of unknown keys.
    if isinstance(manifest, dict) and "repository" in manifest:
        repository = manifest.pop("repository")
        if not isinstance(repository, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
            raise ValueError("release repository is invalid")
    require_keys(manifest, ("schemaVersion", "channel", "version", "commit",
                            "validationMode", "compatibility", "artifacts"),
                 "manifest")
    if manifest["schemaVersion"] != 2 or manifest["channel"] != "stable":
        raise ValueError("manifest is not a stable schema v2 release")
    if not isinstance(manifest["version"], str) or not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", manifest["version"]):
        raise ValueError("release version is invalid")
    if not isinstance(manifest["commit"], str) or \
            not re.fullmatch(r"[0-9a-f]{40}", manifest["commit"]):
        raise ValueError("release commit is invalid")
    if manifest["validationMode"] not in ("github-hosted-no-vm", "physical-apple-silicon"):
        raise ValueError("release validation mode is invalid")
    compatibility = manifest["compatibility"]
    require_keys(compatibility, ("os", "architecture", "minimumMacOS"), "compatibility")
    if compatibility["os"] != "darwin" or compatibility["architecture"] != "arm64":
        raise ValueError("manifest is not compatible with Apple Silicon macOS")
    current = version(sys.argv[2])
    minimum = version(compatibility["minimumMacOS"])
    current = current + (0,) * (3 - len(current))
    minimum = minimum + (0,) * (3 - len(minimum))
    if current < minimum:
        raise ValueError("macOS is below the release minimum")
    if sys.argv[3] not in ("arm64", "arm64e"):
        raise ValueError("host architecture is not Apple Silicon")
    artifacts = manifest["artifacts"]
    require_keys(artifacts, ("host", "guestImage"), "artifacts")
    host_url, host_hash = artifact(artifacts["host"], "host artifact")
    guest_url, guest_hash = artifact(artifacts["guestImage"], "guest image artifact")
except (OSError, TypeError, ValueError, json.JSONDecodeError) as error:
    raise SystemExit("hamn update: invalid immutable release manifest: " + str(error))

print(manifest["version"])
print(host_url)
print(host_hash)
print(guest_url)
print(guest_hash)
'''


def private_json(path, value):
    path.write_text(json.dumps(value) + "\n")
    path.chmod(0o600)


def sha(data):
    return hashlib.sha256(data).hexdigest()


class GeneratedContracts(unittest.TestCase):
    def native_fields(self, path):
        """The shipped client's five-line field view of a manifest file."""
        result = subprocess.run([BINARY, "__install-support", "manifest", path, "13.0", "arm64"],
                                capture_output=True, text=True, timeout=8)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.splitlines()

    def test_legacy_v2_reachability_and_v3_digest_identity(self):
        rng = random.Random(SEED)
        with tempfile.TemporaryDirectory(prefix="hamn-property-manifest-") as temporary:
            path = Path(temporary) / "manifest.json"
            for index in range(16):
                with self.subTest(seed=SEED, case=index):
                    version = "v" + ".".join(str(rng.randrange(1 << 32)) for _ in range(3))
                    value = manifest(rng.randbytes(rng.randrange(1, 4097)), version)
                    value["commit"] = rng.randbytes(20).hex()
                    for name in ("host", "guestImage"):
                        data = rng.randbytes(rng.randrange(1, 8193))
                        value["artifacts"][name].update(
                            url=f"https://fixture.test/{version}/{name}-{index}",
                            sha256=sha(data), size=len(data))
                    v2 = copy.deepcopy(value)
                    v2["schemaVersion"] = 2
                    v2["artifacts"] = {name: {key: artifact[key] for key in ("url", "sha256")}
                                       for name, artifact in value["artifacts"].items()}
                    expected = [version,
                        value["artifacts"]["host"]["url"], value["artifacts"]["host"]["sha256"],
                        value["artifacts"]["guestImage"]["url"], value["artifacts"]["guestImage"]["sha256"]]
                    # The shipped client selects identical bytes from either schema.
                    for item in (v2, value):
                        private_json(path, item)
                        self.assertEqual(self.native_fields(path), expected)
                    private_json(path, v2)
                    output = io.StringIO()
                    with patch.object(sys, "argv", ["legacy", str(path), "13.0", "arm64"]), contextlib.redirect_stdout(output):
                        exec(compile(LEGACY_PARSER, "frozen-v2-consumer", "exec"), {})
                    self.assertEqual(output.getvalue().splitlines(), expected)
                    # A legacy client cannot consume v3's extra size fields;
                    # keeping the separate v2 endpoint is material, not cosmetic.
                    private_json(path, value)
                    with patch.object(sys, "argv", ["legacy", str(path), "13.0", "arm64"]), self.assertRaises(SystemExit):
                        exec(compile(LEGACY_PARSER, "frozen-v2-consumer", "exec"), {})


class GeneratedTransactions(unittest.TestCase):
    def test_network_tripwire_observes_absolute_curl_connections(self):
        with tempfile.TemporaryDirectory(prefix="hamn-network-tripwire-") as temporary:
            with network_tripwire(Path(temporary)) as (port, requests):
                result = subprocess.run(['/usr/bin/curl', '--disable', '--noproxy', '*',
                    '--connect-timeout', '2', '--max-time', '3',
                    f'https://127.0.0.1:{port}/forbidden'], capture_output=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(requests.exists(), 'connection observer was insensitive')

    def snapshot(self, profile):
        result = {}
        for path in [profile, *sorted(profile.rglob("*"))]:
            info = path.lstat()
            result[str(path.relative_to(profile))] = (info.st_mode, info.st_ino,
                info.st_size, info.st_mtime_ns, path.read_bytes() if path.is_file() else None)
        return result

    def test_generated_profile_trees_survive_noop_repair_and_recovery(self):
        rng = random.Random(SEED + 3)
        for case, point in enumerate(("PREPARED", "AFTER_GUEST_SELECTION")):
            with self.subTest(seed=SEED + 3, case=case, interruption=point), \
                    tempfile.TemporaryDirectory(prefix="hamn-property-transaction-") as temporary, \
                    contextlib.ExitStack() as services:
                root = Path(temporary).resolve()
                native = root / "native-hamn"
                shutil.copy2(BINARY, native)
                home = root / "home"
                home.mkdir()
                bindir, datadir = home / "bin", home / "source"
                release = root / "release"
                (release / "bin").mkdir(parents=True)
                version = "1." + ".".join(str(rng.randrange(10000)) for _ in range(2))
                # The existing real-binary CLI suite owns frontend coverage.
                # Only the version is generated; all private support operations
                # execute the frozen real native implementation.
                binary = release / "bin/hamn"
                binary.write_text('#!/bin/sh\nif [ "$#" = 1 ] && [ "$1" = --version ]; then\n'
                                  f'  printf "%s\\n" "hamn {version}"\n'
                                  'elif [ "${1:-}" = __install-support ]; then\n'
                                  f'  exec {shlex.quote(str(native))} "$@"\n'
                                  'else exit 64; fi\n')
                binary.chmod(0o755)
                for directory in ("scripts", "packaging"):
                    shutil.copytree(ROOT / directory, release / directory)
                (release / "packaging/release/update-manifest-url").write_text("https://fixture.test/manifest-v3.json\n")
                archive = root / "host.tar.gz"
                with tarfile.open(archive, "w:gz") as bundle:
                    bundle.add(release, arcname="release")
                guest = root / "guest.img"
                guest.write_bytes(rng.randbytes(rng.randrange(1, 8193)))
                value = manifest(guest.read_bytes(), "v" + version)
                for name, artifact in (("host", archive), ("guestImage", guest)):
                    value["artifacts"][name].update(url=artifact.as_uri(),
                        sha256=sha(artifact.read_bytes()), size=artifact.stat().st_size)
                manifest_path = root / "manifest.json"
                private_json(manifest_path, value)
                env = {**os.environ, "HOME": str(home), "TMPDIR": str(root),
                       "HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS": "1"}
                command = bindir / "hamn"

                def invoke(path=manifest_path, success=True, bootstrap=False, extra_env=None):
                    script = ROOT / "scripts/update-host.sh" if bootstrap else command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
                    args = ["bash", script, "--bindir", bindir, "--datadir", datadir,
                            "--manifest", path, "--output-json"]
                    if bootstrap: args.append("--bootstrap")
                    result = subprocess.run(args, env={**env, **(extra_env or {})},
                                            capture_output=True, timeout=30)
                    self.assertEqual(result.returncode == 0, success, result.stderr.decode(errors="replace"))
                    return json.loads(result.stdout) if success else result

                self.assertEqual(invoke(bootstrap=True)["status"], "updated")
                active = os.readlink(command)
                cache = home / ".hamn/cache"
                selection = cache / "guest-image.json"
                desired = selection.read_bytes()
                profiles = []
                for index in range(3):
                    profile = home / ".hamn" / ("profile space " if index == 1 else "profile-한글-" if index == 2 else "profile-")
                    profile.mkdir()
                    (profile / "nested").mkdir()
                    for filename, size in (("disk.img", rng.randrange(1, 16385)), ("config.json", 0),
                                           ("nested/user-data.bin", rng.randrange(1, 4097))):
                        path = profile / filename
                        path.write_bytes(rng.randbytes(size))
                        path.chmod(rng.choice((0o600, 0o640, 0o400)))
                    profiles.append(profile)
                before_profiles = [self.snapshot(profile) for profile in profiles]

                def assert_preserved():
                    self.assertEqual(os.readlink(command), active)
                    self.assertEqual([self.snapshot(profile) for profile in profiles], before_profiles)

                # Native curl has an absolute path. Observe the actual endpoint
                # instead of relying on PATH interception or reported counters.
                port, requests = services.enter_context(network_tripwire(root))
                env.update(NO_PROXY="127.0.0.1", no_proxy="127.0.0.1")
                for name in ("host", "guestImage"):
                    value["artifacts"][name]["url"] = f"https://127.0.0.1:{port}/forbidden/{name}"
                private_json(manifest_path, value)
                result = invoke()
                self.assertEqual(result["status"], "up-to-date")
                self.assertEqual(result["downloadedBytes"], 0)
                self.assertEqual(result["reusedBytes"], archive.stat().st_size + guest.stat().st_size)
                self.assertEqual(selection.read_bytes(), desired)
                self.assertEqual(list(cache.glob(".hamn-update-*")), [])
                assert_preserved()

                if case == 0:
                    selection.unlink()
                    previous = None
                else:
                    old_payload = rng.randbytes(rng.randrange(1, 1025))
                    old_key = sha(old_payload)
                    old_name = f"hamn-guest-{old_key}.img"
                    (cache / old_name).write_bytes(old_payload)
                    (cache / (old_name + ".verified")).write_text(old_key + "\n")
                    private_json(selection, {"schemaVersion": 1, "file": old_name, "sha256": old_key})
                    previous = selection.read_bytes()
                ready, release_fifo = root / "ready", root / "release-fifo"
                os.mkfifo(ready)
                os.mkfifo(release_fifo)
                ready_fd = os.open(ready, os.O_RDWR | os.O_NONBLOCK)
                child = None
                try:
                    script = command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
                    barrier_env = {**env, f"HAMN_TEST_UPDATE_{point}_READY_FIFO": str(ready),
                                   f"HAMN_TEST_UPDATE_{point}_RELEASE_FIFO": str(release_fifo)}
                    child = subprocess.Popen(["bash", script, "--bindir", bindir, "--datadir", datadir,
                        "--manifest", manifest_path, "--output-json"], env=barrier_env,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    with selectors.DefaultSelector() as selector:
                        selector.register(ready_fd, selectors.EVENT_READ)
                        self.assertTrue(selector.select(20), (point, child.poll()))
                    self.assertEqual(os.read(ready_fd, 64), b"ready\n")
                    journal = cache / ".hamn-update-transaction"
                    self.assertIn("hostMutation=0", (journal / "state").read_text())
                    visible = selection.read_bytes() if selection.exists() else None
                    self.assertEqual(visible, desired if point == "AFTER_GUEST_SELECTION" else previous)
                    assert_preserved()
                    child.send_signal(signal.SIGKILL)
                    child.communicate(timeout=5)
                    self.assertEqual(child.returncode, -signal.SIGKILL)
                    invalid = root / "invalid.json"
                    invalid.write_text("{")
                    invoke(invalid, success=False)
                    self.assertFalse(journal.exists())
                    self.assertEqual(selection.read_bytes() if selection.exists() else None, previous)
                    assert_preserved()
                finally:
                    if child is not None:
                        if child.poll() is None: child.kill()
                        child.communicate(timeout=5)
                    os.close(ready_fd)
                    ready.unlink()
                    release_fifo.unlink()
                repaired = invoke()
                self.assertEqual(repaired["status"], "repaired")
                self.assertEqual(repaired["downloadedBytes"], 0)
                self.assertEqual(selection.read_bytes(), desired)
                self.assertEqual(invoke()["status"], "up-to-date")
                self.assertFalse(requests.exists())
                self.assertEqual(list(cache.glob(".hamn-update-*")), [])
                assert_preserved()


if __name__ == "__main__":
    unittest.main()
