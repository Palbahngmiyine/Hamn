#!/usr/bin/env python3
"""Portable publication faults and generated-input contracts; no image claims."""
import ast
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "guest/image"))
import image_evidence as evidence

spec = importlib.util.spec_from_file_location("image_variations", ROOT / "guest/tests/image_build_variations.py")
variations = importlib.util.module_from_spec(spec)
spec.loader.exec_module(variations)


class BaselineExport(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory(prefix="hamn-baseline-evidence-")
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        self.source = self.root / "source.img"
        self.data = bytes(range(256)) * 19
        self.source.write_bytes(self.data)
        self.report = self.root / "report.json"
        self.report.write_text(json.dumps({"baselineSha256": hashlib.sha256(self.data).hexdigest(),
                                           "baselineCompressedBytes": len(self.data)}))
        self.target = self.root / "baseline.img"
        self.sidecar = self.root / "baseline.img.sha256"

    def assert_no_partial(self):
        self.assertFalse(self.target.exists())
        self.assertFalse(self.sidecar.exists())
        self.assertEqual(list(self.root.glob(".hamn-baseline-*")), [])
        self.assertEqual(self.source.read_bytes(), self.data)
        self.assertEqual(self.source.stat().st_nlink, 1)

    def test_exact_bytes_private_mode_single_link_and_no_overwrite(self):
        evidence.publish(self.source, self.target, self.report)
        self.assertEqual(self.target.read_bytes(), self.data)
        self.assertEqual(self.sidecar.read_text(), hashlib.sha256(self.data).hexdigest() + "  baseline.img\n")
        for path in (self.target, self.sidecar):
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(path.stat().st_nlink, 1)
        with self.assertRaisesRegex(ValueError, "collides"):
            evidence.publish(self.source, self.target, self.report)
        self.assertEqual(self.target.read_bytes(), self.data)

    def test_reserved_paths_symlinks_and_unsafe_parent_rejected(self):
        with self.assertRaises(ValueError): evidence.output_paths(self.target, [self.sidecar])
        self.target.symlink_to(self.source)
        with self.assertRaises(ValueError): evidence.output_paths(self.target)
        self.target.unlink()
        parent = self.root / "unsafe"
        parent.mkdir(mode=0o777)
        parent.chmod(0o777)
        with self.assertRaises(ValueError): evidence.output_paths(parent / "baseline")
        parent.chmod(0o700)
        link = self.root / "linked-parent"
        link.symlink_to(parent, target_is_directory=True)
        with self.assertRaises(ValueError): evidence.output_paths(link / "baseline")
        self.assert_no_partial()

    def test_wrong_hash_size_and_linked_source_never_publish(self):
        for record in ({"baselineSha256": "0" * 64, "baselineCompressedBytes": len(self.data)},
                       {"baselineSha256": hashlib.sha256(self.data).hexdigest(), "baselineCompressedBytes": len(self.data) - 1}):
            self.report.write_text(json.dumps(record))
            with self.assertRaises(ValueError): evidence.publish(self.source, self.target, self.report)
            self.assert_no_partial()
        alias = self.root / "alias"
        os.link(self.source, alias)
        with self.assertRaises(ValueError): evidence.publish(self.source, self.target, self.report)
        alias.unlink()
        self.assert_no_partial()

    def test_every_sync_or_link_failure_rolls_back_only_owned_publication(self):
        for operation, failures in (("fsync", (1, 2, 3)), ("link", (1, 2))):
            for nth in failures:
                with self.subTest(operation=operation, nth=nth):
                    real = getattr(os, operation)
                    calls = 0
                    def failing(*args, **kwargs):
                        nonlocal calls
                        calls += 1
                        if calls == nth: raise OSError("injected publication failure")
                        return real(*args, **kwargs)
                    with patch.object(evidence.os, operation, side_effect=failing), self.assertRaises(OSError):
                        evidence.publish(self.source, self.target, self.report)
                    self.assert_no_partial()
        evidence.publish(self.source, self.target, self.report)
        self.assertEqual(self.target.read_bytes(), self.data)

    def test_publication_race_preserves_competing_artifact(self):
        real = os.link
        def race(source, destination, **kwargs):
            if destination == self.target.name:
                self.target.write_bytes(b"competing evidence")
            return real(source, destination, **kwargs)
        with patch.object(evidence.os, "link", side_effect=race), self.assertRaises(FileExistsError):
            evidence.publish(self.source, self.target, self.report)
        self.assertEqual(self.target.read_bytes(), b"competing evidence")
        self.assertFalse(self.sidecar.exists())
        self.assertEqual(list(self.root.glob(".hamn-baseline-*")), [])


class VariationInputs(unittest.TestCase):
    def test_seed_reproducibility_limits_and_valid_guest_only_script(self):
        cases = variations.variations(20260921, 2)
        self.assertEqual(cases, variations.variations(20260921, 2))
        self.assertNotEqual(cases, variations.variations(20260922, 2))
        self.assertEqual(cases[0]["regenerableBytesPerFile"], 0)
        self.assertTrue(1024 <= cases[1]["regenerableBytesPerFile"] <= 65536)
        for case in cases:
            tree = ast.parse(variations.injection_script(case))
            constants = [item.value for item in ast.walk(tree) if isinstance(item, ast.Constant)]
            self.assertIn("/etc/machine-id", constants)
            self.assertIn("/var/cache/apt/archives/hamn-generated.deb", constants)
            self.assertIn("\n", constants)
        for seed, count in ((-1, 2), (2**32, 2), (0, 0), (0, 5)):
            with self.assertRaises(ValueError): variations.variations(seed, count)


if __name__ == "__main__":
    unittest.main()
