#!/usr/bin/env python3
"""Native upgrade CLI against the frozen Python contract and owned local files.

No release network, shared builds, or VM operations. Local artifact opt-in is
confined to child processes and ephemeral manifests/caches.
"""
import copy
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
from test_upgrade_support import ROOT, manifest, upgrade

BINARY = Path(os.environ.get("HAMN", ROOT / "build/hamn")).resolve()

class NativeUpgrade(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hamn-native-upgrade-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {**os.environ, "HOME": str(self.root), "HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS": "1"}
        self.path = self.root / "manifest.json"
        self.path.write_text(json.dumps(manifest()))
        self.path.chmod(0o600)

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
            self.assertEqual(result.stdout, "0\n")
            expected = upgrade.parse_manifest(self.path.read_bytes(), "13.0", "arm64")
            self.assertEqual(json.loads(output.read_text()), expected)
        return output

    def test_manifest_v2_v3_and_rejections_match_independent_contract(self):
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
        for key, val in (("size", True), ("size", 0), ("size", upgrade.HOST_LIMIT + 1), ("size", None),
                         ("url", "http://example.test/host"), ("sha256", "A" * 64)):
            item = manifest(); item["artifacts"]["host"][key] = val; invalid.append(item)
        for key, val in (("schemaVersion", True), ("version", "v01.2.3"), ("repository", "legacy/hamn"), ("unexpected", 1)):
            item = manifest(); item[key] = val; invalid.append(item)
        invalid += ['{"schemaVersion":3,"schemaVersion":3}', "NaN", "{" + " " * upgrade.MANIFEST_LIMIT]
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
        expected_counts = {path.stem: json.loads(path.read_text()) for path in counts.iterdir()}
        actual = json.loads(self.call("result", self.path, "1.0.0", "updated", counts).stdout)
        self.assertEqual(actual, upgrade.result("1.0.0", value, "updated", expected_counts))
        self.call("reuse-counts", self.path, cache, counts, "both")
        self.assertEqual(json.loads((counts / "host.json").read_text())["reusedBytes"], len(payload.read_bytes()))
        (counts / "host.json").write_text(json.dumps({"downloadedBytes":upgrade.MAX_COUNTER,"resumedBytes":0,"reusedBytes":0,"source":"network"}))
        (counts / "guestImage.json").write_text(json.dumps({"downloadedBytes":1,"resumedBytes":0,"reusedBytes":0,"source":"network"}))
        self.call("result", self.path, "1.0.0", "updated", counts, success=False)

    def test_unsupported_check_is_read_only_and_does_not_validate_network(self):
        before = sorted(self.root.iterdir())
        value = json.loads(self.call("check", "--current-version", "1.0.0-dev", "--manifest", "invalid-url",
            "--macos", "13", "--architecture", "arm64", "--home", self.root).stdout)
        self.assertEqual(value, upgrade.unsupported_result("1.0.0-dev"))
        self.assertEqual(sorted(self.root.iterdir()), before)

    def test_automatic_ttl_and_cross_process_lock_without_network(self):
        cache = upgrade.cache_root(self.root)
        path = cache / "update-check-v1.json"
        record = {"schemaVersion":1,"checkedAt":int(time.time()),"ok":True,"latestVersion":"1.3.0"}
        upgrade.atomic_json(path, record)
        args = ["automatic", "--manifest", "invalid-offline-url", "--current-version", "1.0.0",
                "--home", self.root, "--macos", "13", "--architecture", "arm64"]
        before = path.read_bytes(); self.call(*args); self.assertEqual(path.read_bytes(), before)
        record["checkedAt"] -= 86460; upgrade.atomic_json(path, record)
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
