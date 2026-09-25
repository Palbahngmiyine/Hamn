#!/usr/bin/env python3
"""Native upgrade CLI against contract-derived expectations and owned local files.

Expected values come from the documented manifest/result contracts
(docs/INSTALLATION.md), not from a second implementation. No release network,
shared builds, or VM operations. Local artifact opt-in is confined to child
processes and ephemeral manifests/caches.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get("HAMN", ROOT / "build/hamn")).resolve()
MANIFEST_LIMIT = 256 * 1024  # manifests are limited to 256 KiB
HOST_LIMIT = 128 * 1024 * 1024  # host archives are limited to 128 MiB
MAX_COUNTER = (1 << 64) - 1  # byte counters are checked unsigned 64-bit totals
COUNTERS = ("downloadedBytes", "resumedBytes", "reusedBytes")


def manifest(payload=b"release", version="v1.2.3"):
    """A valid stable schema v3 release naming `payload` for both artifacts."""
    artifact = {"url": "https://fixture.test/artifact",
                "sha256": hashlib.sha256(payload).hexdigest(), "size": len(payload)}
    return {"schemaVersion": 3, "channel": "stable", "version": version, "commit": "a" * 40,
            "validationMode": "github-hosted-no-vm",
            "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
            "artifacts": {"host": artifact, "guestImage": {**artifact, "format": "qcow2",
                          "compression": "zlib", "virtualSize": 8 * 1024 ** 3}}}


def private_json(path, value):
    path.write_text(json.dumps(value) + "\n")
    path.chmod(0o600)


def expected_result(current, value, status, counts):
    """The documented result object: per-source records plus checked totals."""
    empty = {"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0, "source": "none"}
    artifacts = {name: counts.get(name, empty) for name in ("manifest", "host", "guestImage")}
    return {"schemaVersion": 1, "currentVersion": current.removeprefix("v"),
            "latestVersion": value["version"].removeprefix("v"), "status": status,
            **{field: sum(item[field] for item in artifacts.values()) for field in COUNTERS},
            "artifacts": artifacts, "profileDisksChanged": False, "completed": True}


class NativeUpgrade(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hamn-native-upgrade-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {**os.environ, "HOME": str(self.root), "HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS": "1"}
        self.path = self.root / "manifest.json"
        private_json(self.path, manifest())

    def call(self, *args, success=True):
        result = subprocess.run([BINARY, "__install-support", "upgrade", *map(str, args)],
            env=self.env, capture_output=True, text=True, timeout=8)
        self.assertEqual(result.returncode == 0, success, result.stderr)
        return result

    def metadata(self, value, success=True):
        self.path.write_text(value if isinstance(value, str) else json.dumps(value))
        output = self.root / "parsed.json"
        result = self.call("manifest", "--manifest", self.path.as_uri(), "--macos", "13.0",
            "--architecture", "arm64", "--output", output, success=success)
        if success:
            # Local manifests transfer no network bytes. The accepted manifest is
            # recorded unchanged except for the dropped legacy v2 repository key.
            self.assertEqual(result.stdout, "0\n")
            expected = {key: item for key, item in value.items() if key != "repository"}
            self.assertEqual(json.loads(output.read_text()), expected)
        return output

    def test_manifest_v2_v3_and_rejections_follow_the_published_contract(self):
        for schema in (2, 3):
            value = manifest()
            if schema == 2:
                value["schemaVersion"] = 2
                value["repository"] = "legacy/hamn"
                value["artifacts"] = {name: {key: item[key] for key in ("url", "sha256")}
                    for name, item in value["artifacts"].items()}
            self.metadata(value)
        output = self.root / "parsed.json"
        original = output.read_bytes()
        invalid = []
        for key, val in (("size", True), ("size", 0), ("size", HOST_LIMIT + 1), ("size", None),
                         ("url", "http://example.test/host"), ("sha256", "A" * 64)):
            item = manifest(); item["artifacts"]["host"][key] = val; invalid.append(item)
        for key, val in (("schemaVersion", True), ("version", "v01.2.3"), ("repository", "legacy/hamn"), ("unexpected", 1)):
            item = manifest(); item[key] = val; invalid.append(item)
        invalid += ['{"schemaVersion":3,"schemaVersion":3}', "NaN", "{" + " " * MANIFEST_LIMIT]
        for item in invalid:
            with self.subTest(item=str(item)[:120]):
                self.metadata(item, success=False)
                self.assertEqual(output.read_bytes(), original)

    def test_canonical_versions_compare_numerically_and_reject_overflow(self):
        # Feature: automatic-upgrade-and-image-optimization, Property 1: Stable Semantic Version Ordering
        rng = random.Random(20260921)
        for _ in range(100):
            current = tuple(rng.randrange(100) for _ in range(3))
            latest = tuple(rng.randrange(100) for _ in range(3))
            value = manifest(version="v" + ".".join(map(str, latest)))
            self.path.write_text(json.dumps(value))
            actual = self.call("status", self.path, ".".join(map(str, current)), self.root).stdout.strip()
            self.assertEqual(actual, "update-available" if latest > current else "ahead" if latest < current else "repair-required")
        for value in ("1.2.3", "v4294967295.0.0"):
            self.call("version", value)
        for value in ("1.2", "01.2.3", "1.2.3-rc.1", "1.2.3+build", "4294967296.0.0"):
            self.call("version", value, success=False)

    def test_local_acquisition_reuse_and_checked_accounting(self):
        payload = self.root / "payload"; payload.write_bytes(b"owned artifact")
        value = manifest(payload.read_bytes())
        for artifact in value["artifacts"].values(): artifact["url"] = payload.as_uri()
        self.path.write_text(json.dumps(value))
        cache = self.root / "cache"; cache.mkdir(mode=0o755)
        counts = self.root / "counts"; counts.mkdir(mode=0o700)
        for name in ("host", "guestImage"):
            output = self.call("acquire", self.path, name, cache, counts / (name + ".json"))
            self.assertEqual(Path(output.stdout.strip()).read_bytes(), payload.read_bytes())
        recorded = {path.stem: json.loads(path.read_text()) for path in counts.iterdir()}
        self.assertEqual(set(recorded), {"host", "guestImage"})
        # A local copy transfers no network bytes. Both artifacts name the same
        # digest, so the second acquisition reuses the content-addressed file.
        for name, source in (("host", "local"), ("guestImage", "cache")):
            self.assertEqual(recorded[name], {"downloadedBytes": 0, "resumedBytes": 0,
                                              "reusedBytes": len(payload.read_bytes()), "source": source}, name)
        actual = json.loads(self.call("result", self.path, "1.0.0", "updated", counts).stdout)
        self.assertEqual(actual, expected_result("1.0.0", value, "updated", recorded))
        self.call("reuse-counts", self.path, cache, counts, "both")
        self.assertEqual(json.loads((counts / "host.json").read_text())["reusedBytes"], len(payload.read_bytes()))
        (counts / "host.json").write_text(json.dumps({"downloadedBytes":MAX_COUNTER,"resumedBytes":0,"reusedBytes":0,"source":"network"}))
        (counts / "guestImage.json").write_text(json.dumps({"downloadedBytes":1,"resumedBytes":0,"reusedBytes":0,"source":"network"}))
        self.call("result", self.path, "1.0.0", "updated", counts, success=False)

    def test_unsupported_check_is_read_only_and_does_not_validate_network(self):
        before = sorted(self.root.iterdir())
        value = json.loads(self.call("check", "--current-version", "1.0.0-dev", "--manifest", "invalid-url",
            "--macos", "13", "--architecture", "arm64", "--home", self.root).stdout)
        empty = {"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0, "source": "none"}
        self.assertEqual(value, {"schemaVersion": 1, "currentVersion": "1.0.0-dev", "latestVersion": None,
            "status": "unsupported-install", "downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0,
            "artifacts": {"manifest": empty, "host": empty, "guestImage": empty},
            "profileDisksChanged": False, "completed": True})
        self.assertEqual(sorted(self.root.iterdir()), before)

    def test_automatic_ttl_and_cross_process_lock_without_network(self):
        (self.root / ".hamn").mkdir(mode=0o700)
        cache = self.root / ".hamn/cache"
        cache.mkdir(mode=0o755)
        path = cache / "update-check-v1.json"
        record = {"schemaVersion":1,"checkedAt":int(time.time()),"ok":True,"latestVersion":"1.3.0"}
        private_json(path, record)
        args = ["automatic", "--manifest", "invalid-offline-url", "--current-version", "1.0.0",
                "--home", self.root, "--macos", "13", "--architecture", "arm64"]
        before = path.read_bytes(); self.call(*args); self.assertEqual(path.read_bytes(), before)
        record["checkedAt"] -= 86460; private_json(path, record)
        with (cache / ".update-check.lock").open("r+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.call(*args, success=False)
            self.assertEqual(json.loads(path.read_text()), record)
        self.call(*args)
        updated = json.loads(path.read_text())
        self.assertFalse(updated["ok"]); self.assertEqual(updated["latestVersion"], "1.3.0")
        before = path.read_bytes(); self.call(*args); self.assertEqual(path.read_bytes(), before)
        self.assertEqual({item.name for item in (self.root / ".hamn").iterdir()}, {"cache"})

if __name__ == "__main__":
    unittest.main()
