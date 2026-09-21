#!/usr/bin/env python3
"""Release contracts and independently counted HTTP transfer regressions.

The fixture rewrites only its synthetic HTTPS origin to loopback HTTP after
asserting production curl protocol restrictions. It never changes production
policy or contacts a release service. No shared build output is touched.
"""
import contextlib
import copy
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import random
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("upgrade_support", ROOT / "scripts/upgrade_support.py")
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)


def manifest(payload=b"release", version="v1.2.3"):
    artifact = {"url": "https://fixture.test/artifact", "sha256": hashlib.sha256(payload).hexdigest(), "size": len(payload)}
    return {"schemaVersion": 3, "channel": "stable", "version": version, "commit": "a" * 40,
            "validationMode": "github-hosted-no-vm", "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
            "artifacts": {"host": artifact, "guestImage": {**artifact, "format": "qcow2", "compression": "zlib", "virtualSize": 8 * 1024 ** 3}}}


class Contracts(unittest.TestCase):
    def test_semver_numeric_order_and_rejections(self):
        # Feature: automatic-upgrade-and-image-optimization, Property 1: Stable Semantic Version Ordering
        rng = random.Random(20260921)
        for _ in range(200):
            left = tuple(rng.randrange(2 ** 32) for _ in range(3))
            right = tuple(rng.randrange(2 ** 32) for _ in range(3))
            self.assertEqual(upgrade.stable_version("v" + ".".join(map(str, left))) < upgrade.stable_version(".".join(map(str, right))), left < right)
        for invalid in ["", "1", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "1.2.03", "1.2.3-rc.1", "1.2.3+build", " 1.2.3", "1.2.3\n", "4294967296.0.0", "V1.2.3", True]:
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                upgrade.stable_version(invalid)

    def test_actual_contract_v2_v3_and_invalid_inputs(self):
        valid = manifest()
        self.assertEqual(upgrade.parse_manifest(json.dumps(valid).encode(), "13", "arm64"), valid)
        v2 = copy.deepcopy(valid)
        v2["schemaVersion"] = 2
        for name, artifact in v2["artifacts"].items():
            v2["artifacts"][name] = {key: artifact[key] for key in ("url", "sha256")}
        v2["repository"] = "legacy/hamn"
        self.assertNotIn("repository", upgrade.parse_manifest(json.dumps(v2).encode(), "13.1", "arm64"))
        cases = []
        for key, value in [("schemaVersion", True), ("unexpected", 1), ("version", "v01.2.3"), ("repository", "bad/extra/name")]:
            item = copy.deepcopy(valid); item[key] = value; cases.append(item)
        for key, value in [("size", True), ("size", 0), ("size", upgrade.HOST_LIMIT + 1), ("url", "http://example.com/payload"), ("sha256", "A" * 64)]:
            item = copy.deepcopy(valid); item["artifacts"]["host"][key] = value; cases.append(item)
        for item in cases:
            with self.subTest(item=item), self.assertRaises(ValueError):
                upgrade.parse_manifest(json.dumps(item).encode(), "13.0", "arm64")
        for data in [b'{"schemaVersion":3,"schemaVersion":3}', b'{' + b' ' * upgrade.MANIFEST_LIMIT, b'NaN']:
            with self.assertRaises(ValueError): upgrade.parse_manifest(data, "13", "arm64")
        for macos, architecture in [("12.6", "arm64"), ("13", "x86_64")]:
            with self.assertRaises(ValueError): upgrade.parse_manifest(json.dumps(valid).encode(), macos, architecture)

    def test_accounting_overflow_is_rejected(self):
        with self.assertRaises(ValueError):
            upgrade.result("1.0.0", manifest(), "updated", {"host":{"downloadedBytes":upgrade.MAX_COUNTER}, "guestImage":{"downloadedBytes":1}})

    def test_development_check_returns_unsupported_without_state_or_network(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = subprocess.run([sys.executable, ROOT / "scripts/upgrade_support.py", "check",
                "--current-version", "0.1.1-dev", "--manifest", "not-a-network-url",
                "--macos", "13.0", "--architecture", "arm64", "--home", temporary],
                capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            value = json.loads(result.stdout)
            self.assertEqual(value["status"], "unsupported-install")
            self.assertEqual(value["downloadedBytes"], 0)
            self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_check_isolation_and_future_cache(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            cache = upgrade.cache_root(home)
            upgrade.atomic_json(cache / "update-check-v1.json", {"schemaVersion":1,"checkedAt":1001,"ok":True,"latestVersion":"1.2.3"})
            self.assertIsNone(upgrade.read_check(cache, 1000))
            self.assertEqual(upgrade.version_status("1.2.2", manifest(), cache), "update-available")
            self.assertEqual(upgrade.version_status("1.2.3", manifest(), cache), "repair-required")
            self.assertEqual(upgrade.version_status("1.2.4", manifest(), cache), "ahead")

    def test_automatic_ttl_failure_backoff_and_unsafe_cache(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            args = types.SimpleNamespace(current_version="1.0.0", home=home,
                manifest="https://fixture.test/manifest", macos="13.0", architecture="arm64")
            with mock.patch.object(upgrade.time, "time", return_value=100000), mock.patch.object(upgrade, "fetch_manifest", return_value=(manifest(), 100)) as fetch:
                upgrade.automatic(args); upgrade.automatic(args)
                self.assertEqual(fetch.call_count, 1)
                self.assertTrue(fetch.call_args.kwargs["automatic"])
            cache = home / ".hamn/cache"
            with mock.patch.object(upgrade.time, "time", return_value=200000), mock.patch.object(upgrade, "fetch_manifest", side_effect=OSError("offline")) as fetch:
                upgrade.automatic(args); upgrade.automatic(args)
                self.assertEqual(fetch.call_count, 1)
            record = upgrade.read_check(cache, 200000)
            self.assertFalse(record["ok"])
            self.assertEqual(record["latestVersion"], "1.2.3")
            with mock.patch.object(upgrade.time, "time", return_value=221601), mock.patch.object(upgrade, "fetch_manifest", return_value=(manifest(), 100)) as fetch:
                upgrade.automatic(args)
                self.assertEqual(fetch.call_count, 1)
            external = home / "external"; external.write_text("preserved")
            (cache / "update-check-v1.json").unlink()
            (cache / "update-check-v1.json").symlink_to(external)
            with mock.patch.object(upgrade, "fetch_manifest", return_value=(manifest(), 100)), self.assertRaises(ValueError):
                upgrade.automatic(args)
            self.assertEqual(external.read_text(), "preserved")


class HttpFixture:
    def __init__(self, payload):
        self.payload = payload
        self.requests = []
        self.body_bytes = 0
        self.mode = "normal"
        self.mutex = threading.Lock()
        outer = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_GET(self):
                with outer.mutex:
                    outer.requests.append({"range":self.headers.get("Range"), "ifRange":self.headers.get("If-Range")})
                offset = 0
                status = 200
                if self.headers.get("Range") and outer.mode != "ignore":
                    offset = int(self.headers["Range"].split("=")[1].split("-")[0]); status = 206
                body = outer.payload[offset:]
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("ETag", '"fixture-v1"')
                if status == 206: self.send_header("Content-Range", f"bytes {offset}-{len(outer.payload)-1}/{len(outer.payload)}")
                self.end_headers()
                if outer.mode == "interrupt": body = body[:1000]
                if outer.mode == "oversize": body += b"overflow"
                try: self.wfile.write(body); self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError): pass
                with outer.mutex: outer.body_bytes += len(body)
                self.close_connection = True
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self): self.thread.start(); return self
    def __exit__(self, *args): self.server.shutdown(); self.server.server_close(); self.thread.join()


class Acquisition(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="hamn-upgrade-contract-")
        self.root = Path(self.temporary.name)
        self.cache = upgrade.cache_root(self.root)
        self.payload = bytes(range(256)) * 1024
        self.artifact = manifest(self.payload)["artifacts"]["guestImage"]
        self.fixture = HttpFixture(self.payload).__enter__()
        self.transport = self.root / "transport"; self.transport.mkdir()
        shim = self.transport / "curl"
        shim.write_text(f"#!{sys.executable}\n" + f'''import os,sys
args=sys.argv[1:]
assert args[args.index('--proto')+1] == '=https'
assert args[args.index('--proto-redir')+1] == '=https'
assert args[-1].startswith('https://fixture.test/')
for key in ('--proto','--proto-redir'):
    args[args.index(key)+1]='=http'
args[-1]=args[-1].replace('https://fixture.test','http://127.0.0.1:{self.fixture.server.server_port}')
os.execv('/usr/bin/curl',['curl']+args)
'''); shim.chmod(0o755)
        self.old_path = os.environ.get("PATH", "")
        os.environ["PATH"] = str(self.transport) + ":" + self.old_path

    def tearDown(self):
        os.environ["PATH"] = self.old_path
        self.fixture.__exit__()
        self.temporary.cleanup()

    def test_cold_warm_and_corruption_repair(self):
        # Property 4: Content-Addressed Artifact Safety; Property 12: Transfer Accounting Conservation
        path, counts = upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.assertEqual(path.read_bytes(), self.payload)
        self.assertEqual(counts["downloadedBytes"], len(self.payload))
        self.assertEqual(self.fixture.body_bytes, len(self.payload))
        path2, counts = upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.assertEqual(path2, path)
        self.assertEqual(counts["reusedBytes"], len(self.payload))
        self.assertEqual(len(self.fixture.requests), 1)
        path.write_bytes(b"damaged")
        repaired, _ = upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.assertEqual(repaired.read_bytes(), self.payload)

    def test_resume_only_missing_bytes(self):
        # Feature: automatic-upgrade-and-image-optimization, Property 6: Resume Transfers Only Missing Bytes
        self.fixture.mode = "interrupt"
        with self.assertRaises(OSError): upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.fixture.mode = "normal"
        path, counts = upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.assertEqual(path.read_bytes(), self.payload)
        self.assertEqual(self.fixture.requests[-1]["range"], "bytes=1000-")
        self.assertEqual(self.fixture.requests[-1]["ifRange"], '"fixture-v1"')
        self.assertEqual(self.fixture.body_bytes, len(self.payload))
        self.assertEqual(counts["downloadedBytes"], len(self.payload) - 1000)
        self.assertEqual(counts["reusedBytes"], 1000)

    def test_ignored_range_has_one_clean_retry(self):
        self.fixture.mode = "interrupt"
        with self.assertRaises(OSError): upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.fixture.mode = "ignore"
        path, counts = upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.assertEqual(path.read_bytes(), self.payload)
        self.assertEqual([entry["range"] for entry in self.fixture.requests], [None, "bytes=1000-", None])
        self.assertEqual(counts["downloadedBytes"], 2 * len(self.payload))

    def test_complete_partial_survives_publication_failure_without_redownload(self):
        replace = os.replace
        def fail_publish(source, destination):
            if str(source).endswith(".partial"):
                raise OSError("injected rename failure")
            return replace(source, destination)
        with mock.patch.object(upgrade.os, "replace", fail_publish), self.assertRaises(OSError):
            upgrade.acquire(self.cache, self.artifact, "guestImage")
        path, counts = upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.assertEqual(path.read_bytes(), self.payload)
        self.assertEqual(counts["downloadedBytes"], 0)
        self.assertEqual(counts["reusedBytes"], len(self.payload))
        self.assertEqual(self.fixture.body_bytes, len(self.payload))

    def test_digest_size_and_unsafe_cache_rejection(self):
        bad = {**self.artifact, "sha256":"0" * 64}
        with self.assertRaises(ValueError): upgrade.acquire(self.cache, bad, "guestImage")
        self.assertFalse((self.cache / "downloads" / ("." + "0" * 64 + ".partial")).exists())
        bad = {**self.artifact, "size":len(self.payload) - 1}
        with self.assertRaises(ValueError): upgrade.acquire(self.cache, bad, "guestImage")
        external = self.root / "external"; external.write_bytes(b"preserve")
        key = self.artifact["sha256"]
        final = self.cache / "downloads" / (key + ".artifact")
        final.symlink_to(external)
        with self.assertRaises(ValueError): upgrade.acquire(self.cache, self.artifact, "guestImage")
        self.assertEqual(external.read_bytes(), b"preserve")

    def test_single_flight(self):
        # Feature: automatic-upgrade-and-image-optimization, Property 5: Download Single-Flight
        path = self.root / "manifest.json"; path.write_text(json.dumps(manifest(self.payload)))
        children = [subprocess.Popen([sys.executable, ROOT / "scripts/upgrade_support.py", "acquire", path,
            "guestImage", self.cache, self.root / f"counts-{index}.json"], stdout=subprocess.PIPE, stderr=subprocess.PIPE) for index in range(2)]
        results = [child.communicate(timeout=15) for child in children]
        for child, result in zip(children, results): self.assertEqual(child.returncode, 0, result)
        self.assertEqual(results[0][0], results[1][0])
        self.assertEqual(len(self.fixture.requests), 1)
        self.assertEqual(self.fixture.body_bytes, len(self.payload))


if __name__ == "__main__":
    unittest.main()
