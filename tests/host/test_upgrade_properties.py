#!/usr/bin/env python3
"""Bounded generated upgrade cases, seed 20260921.

These complement the fixed fault/signal matrices; they are not a proof over all
inputs or a guest boot/functional-equivalence test. All installs, HTTP traffic,
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
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from test_upgrade_support import HttpFixture, ROOT, manifest, upgrade

SEED = 20260921
SUPPORT = ROOT / "scripts/upgrade_support.py"

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


@contextlib.contextmanager
def transfer_fixture(root, payload):
    with HttpFixture(payload) as server:
        transport = root / "transport"
        transport.mkdir()
        shim = transport / "curl"
        shim.write_text(f"#!{sys.executable}\n" + f'''import os, sys
args = sys.argv[1:]
assert args[args.index('--proto') + 1] == '=https'
assert args[args.index('--proto-redir') + 1] == '=https'
assert args[-1].startswith('https://fixture.test/')
for key in ('--proto', '--proto-redir'):
    args[args.index(key) + 1] = '=http'
args[-1] = args[-1].replace('https://fixture.test', 'http://127.0.0.1:{server.server.server_port}')
os.execv('/usr/bin/curl', ['curl'] + args)
''')
        shim.chmod(0o755)
        with patch.dict(os.environ, PATH=f"{transport}:{os.environ.get('PATH', '')}"):
            yield server


class GeneratedContracts(unittest.TestCase):
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
                    parsed = [upgrade.parse_manifest(json.dumps(item).encode(), "13.0", "arm64")
                              for item in (v2, value)]
                    for name in ("host", "guestImage"):
                        for key in ("url", "sha256"):
                            self.assertEqual(parsed[0]["artifacts"][name][key], parsed[1]["artifacts"][name][key])
                    private_json(path, v2)
                    output = io.StringIO()
                    with patch.object(sys, "argv", ["legacy", str(path), "13.0", "arm64"]), contextlib.redirect_stdout(output):
                        exec(compile(LEGACY_PARSER, "frozen-v2-consumer", "exec"), {})
                    self.assertEqual(output.getvalue().splitlines(), [version,
                        value["artifacts"]["host"]["url"], value["artifacts"]["host"]["sha256"],
                        value["artifacts"]["guestImage"]["url"], value["artifacts"]["guestImage"]["sha256"]])
                    # A legacy client cannot consume v3's extra size fields;
                    # keeping the separate v2 endpoint is material, not cosmetic.
                    private_json(path, value)
                    with patch.object(sys, "argv", ["legacy", str(path), "13.0", "arm64"]), self.assertRaises(SystemExit):
                        exec(compile(LEGACY_PARSER, "frozen-v2-consumer", "exec"), {})

    def test_generated_accounting_sums_all_sources_and_rejects_overflow(self):
        rng = random.Random(SEED + 4)
        for case in range(24):
            with self.subTest(seed=SEED + 4, case=case):
                total = rng.randrange(1 << 20) if case < 12 else upgrade.MAX_COUNTER + (case % 5) - 2
                cuts = sorted((0, rng.randrange(total + 1), rng.randrange(total + 1), total))
                received = [right - left for left, right in zip(cuts, cuts[1:])]
                records = {name: {"downloadedBytes": count, "resumedBytes": rng.randrange(count + 1),
                                  "reusedBytes": rng.randrange(65537), "source": "generated"}
                           for name, count in zip(("manifest", "host", "guestImage"), received)}
                expected = {field: sum(item[field] for item in records.values())
                            for field in ("downloadedBytes", "resumedBytes", "reusedBytes")}
                if total > upgrade.MAX_COUNTER:
                    with self.assertRaises(ValueError): upgrade.result("1.2.2", manifest(), "updated", records)
                else:
                    actual = upgrade.result("1.2.2", manifest(), "updated", records)
                    self.assertEqual({field: actual[field] for field in expected}, expected)
                    self.assertEqual(actual["artifacts"], records)


class GeneratedAcquisition(unittest.TestCase):
    def test_generated_partial_cache_states_and_byte_conservation(self):
        rng = random.Random(SEED + 1)
        cases = [("cold", 1), ("cold", 65537), ("resume", 2), ("resume", 4097),
                 ("resume", 65536), ("complete", 31), ("corrupt-final", 8193),
                 ("corrupt-prefix", 257), ("ignored-range", 2049), ("legacy-partial", 4096)]
        with tempfile.TemporaryDirectory(prefix="hamn-property-transfer-") as temporary:
            root = Path(temporary)
            with transfer_fixture(root, b"unused") as server:
                for index, (state, size) in enumerate(cases):
                    with self.subTest(seed=SEED + 1, case=index, state=state, size=size):
                        home = root / str(index)
                        home.mkdir()
                        cache = upgrade.cache_root(home)
                        downloads = cache / "downloads"
                        downloads.mkdir(mode=0o700)
                        payload = rng.randbytes(size)
                        server.payload, server.mode = payload, "normal"
                        name = "host" if index % 2 else "guestImage"
                        artifact = manifest(payload)["artifacts"][name]
                        if state == "legacy-partial":
                            artifact = {key: artifact[key] for key in ("url", "sha256")}
                        key = artifact["sha256"]
                        partial = downloads / f".{key}.partial"
                        final = downloads / f"{key}.artifact"
                        offset = 0
                        if state in ("resume", "complete", "corrupt-prefix", "ignored-range", "legacy-partial"):
                            offset = size if state == "complete" else (1 if index == 2 else rng.randrange(1, size))
                            prefix = bytearray(payload[:offset])
                            if state == "corrupt-prefix": prefix[rng.randrange(offset)] ^= 0xff
                            partial.write_bytes(prefix)
                            partial.chmod(0o600)
                            private_json(downloads / f".{key}.validator", {
                                "schemaVersion": 1, "sha256": key, "size": size, "validator": '"fixture-v1"'})
                        if state == "corrupt-final":
                            final.write_bytes(payload[:-1] + bytes([payload[-1] ^ 0xff]))
                            final.chmod(0o600)
                        if state == "ignored-range": server.mode = "ignore"
                        before_bytes, before_requests = server.body_bytes, len(server.requests)
                        if state == "corrupt-prefix":
                            with self.assertRaises(ValueError): upgrade.acquire(cache, artifact, name)
                            self.assertFalse(final.exists())
                            self.assertFalse(partial.exists())
                            self.assertFalse((downloads / f".{key}.validator").exists())
                            self.assertEqual(server.body_bytes - before_bytes, size - offset)
                            before_bytes, before_requests, offset = server.body_bytes, len(server.requests), 0
                        path, counts = upgrade.acquire(cache, artifact, name)
                        self.assertEqual(path.read_bytes(), payload)
                        self.assertEqual(sha(path.read_bytes()), key)
                        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                        self.assertFalse(partial.exists())
                        downloaded = 0 if state == "complete" else size - offset if state == "resume" else 2 * size if state == "ignored-range" else size
                        reused = offset if state in ("resume", "complete") else 0
                        self.assertEqual(counts["downloadedBytes"], downloaded)
                        self.assertEqual(counts["downloadedBytes"], server.body_bytes - before_bytes)
                        self.assertEqual(counts["resumedBytes"], downloaded if state == "resume" else 0)
                        self.assertEqual(counts["reusedBytes"], reused)
                        self.assertEqual(downloaded + reused, 2 * size if state == "ignored-range" else size)
                        if state == "resume":
                            self.assertEqual(server.requests[before_requests], {"range": f"bytes={offset}-", "ifRange": '"fixture-v1"'})
                        if state == "legacy-partial": self.assertIsNone(server.requests[before_requests]["range"])
                        requests = len(server.requests)
                        _, warm = upgrade.acquire(cache, artifact, name)
                        self.assertEqual(len(server.requests), requests)
                        self.assertEqual((warm["downloadedBytes"], warm["resumedBytes"], warm["reusedBytes"]), (0, 0, size))
                        summary = upgrade.result("1.2.2", manifest(payload), "updated", {name: counts})
                        for field in ("downloadedBytes", "resumedBytes", "reusedBytes"):
                            self.assertEqual(summary[field], counts[field])

    def test_generated_payloads_single_flight_across_processes(self):
        rng = random.Random(SEED + 2)
        with tempfile.TemporaryDirectory(prefix="hamn-property-flight-") as temporary:
            root = Path(temporary)
            with transfer_fixture(root, b"unused") as server:
                for width in (2, 3, 4):
                    with self.subTest(seed=SEED + 2, processes=width):
                        payload = rng.randbytes(rng.randrange(4096, 32769))
                        server.payload = payload
                        home = root / str(width)
                        home.mkdir()
                        cache = upgrade.cache_root(home)
                        path = home / "manifest.json"
                        private_json(path, manifest(payload))
                        before_bytes, before_requests = server.body_bytes, len(server.requests)
                        children = []
                        try:
                            for index in range(width):
                                children.append(subprocess.Popen([sys.executable, SUPPORT, "acquire", path,
                                    "guestImage", cache, home / f"counts-{index}.json"], stdout=subprocess.PIPE, stderr=subprocess.PIPE))
                            results = [child.communicate(timeout=15) for child in children]
                            for child, result in zip(children, results): self.assertEqual(child.returncode, 0, result)
                            self.assertEqual(len({result[0] for result in results}), 1)
                            self.assertEqual(Path(results[0][0].decode().strip()).read_bytes(), payload)
                            self.assertEqual(server.body_bytes - before_bytes, len(payload))
                            self.assertEqual(len(server.requests) - before_requests, 1)
                            records = [json.loads((home / f"counts-{index}.json").read_text()) for index in range(width)]
                            self.assertEqual(sum(record["downloadedBytes"] for record in records), len(payload))
                            self.assertEqual(sum(record["reusedBytes"] for record in records), (width - 1) * len(payload))
                        finally:
                            for child in children:
                                if child.poll() is None: child.kill()
                                child.communicate(timeout=5)


class GeneratedTransactions(unittest.TestCase):
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
                    tempfile.TemporaryDirectory(prefix="hamn-property-transaction-") as temporary:
                root = Path(temporary).resolve()
                home = root / "home"
                home.mkdir()
                bindir, datadir = home / "bin", home / "source"
                release = root / "release"
                (release / "bin").mkdir(parents=True)
                version = "1." + ".".join(str(rng.randrange(10000)) for _ in range(2))
                # The existing real-binary CLI suite owns frontend coverage.
                # This executable supplies only the installer's version contract;
                # install-host/update-host/receipt/journal code remains unchanged.
                binary = release / "bin/hamn"
                binary.write_text(f'#!/bin/sh\n[ "$#" = 1 ] && [ "$1" = --version ] || exit 64\nprintf "%s\\n" "hamn {version}"\n')
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

                # A network trap makes zero Payload independent of reported
                # counters. Healthy/cached states must never call curl.
                transport = root / "transport"
                transport.mkdir()
                requests = root / "unexpected-request"
                curl = transport / "curl"
                curl.write_text(f'#!{sys.executable}\nfrom pathlib import Path\nPath({str(requests)!r}).write_text("unexpected network")\nraise SystemExit(97)\n')
                curl.chmod(0o755)
                env["PATH"] = f"{transport}:{os.environ.get('PATH', '')}"
                for name in ("host", "guestImage"):
                    value["artifacts"][name]["url"] = f"https://fixture.test/forbidden/{name}"
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
