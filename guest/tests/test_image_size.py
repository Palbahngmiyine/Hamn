#!/usr/bin/env python3
"""Contract-derived synthetic size gates; these are not measured image savings."""
import importlib.util
import json
import pathlib
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("image_size", ROOT / "guest/image/verify-size.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
variation_spec = importlib.util.spec_from_file_location("image_variations", ROOT / "guest/tests/image_build_variations.py")
variations = importlib.util.module_from_spec(variation_spec)
variation_spec.loader.exec_module(variations)


class ImageSizeGate(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = pathlib.Path(self.work.name)
        self.baseline = self.root / "baseline"
        self.candidate = self.root / "candidate"
        self.resize(self.baseline, 128 * 1024**2)
        self.resize(self.candidate, 64 * 1024**2)
        self.packages = self.root / "packages.tsv"
        self.packages.write_text("docker.io\t28.0\t12345\n")
        self.report = self.root / "report.json"
        self.budget = self.root / "budget.json"
        self.args = ["verify-size", "--baseline", str(self.baseline), "--candidate", str(self.candidate),
                     "--packages-before", str(self.packages), "--packages-after", str(self.packages),
                     "--report", str(self.report), "--budget", str(self.budget),
                     "--base-sha256", "a" * 64, "--source-revision", "b" * 40]

    def resize(self, path, size):
        with path.open("wb") as output:
            output.truncate(size)

    def run_gate(self, review=False):
        with patch.object(sys, "argv", self.args + (["--review-only"] if review else [])):
            module.main()

    def test_first_result_requires_reviewed_budget(self):
        with self.assertRaisesRegex(ValueError, "reviewed release-size-budget"):
            self.run_gate()
        self.assertTrue(self.report.is_file())
        self.assertFalse(self.budget.exists())
        self.run_gate(review=True)
        proposal = self.report.with_suffix(".budget-proposal.json")
        self.assertTrue(json.loads(self.report.read_text())["reviewOnly"])
        self.assertFalse(self.budget.exists())
        self.budget.write_bytes(proposal.read_bytes())
        self.run_gate()
        self.assertFalse(json.loads(self.report.read_text())["reviewOnly"])

    def test_savings_and_release_limits_cannot_be_bypassed_by_review_mode(self):
        self.resize(self.candidate, 64 * 1024**2 + 1)
        with self.assertRaisesRegex(ValueError, "savings"):
            self.run_gate(review=True)
        self.resize(self.candidate, 2**31)
        self.resize(self.baseline, 3 * 1024**3)
        with self.assertRaisesRegex(ValueError, "asset limit"):
            self.run_gate(review=True)

    def test_budget_overrun_unknown_and_duplicate_keys_rejected(self):
        self.run_gate(review=True)
        value = json.loads(self.report.with_suffix(".budget-proposal.json").read_text())
        value["maximumCompressedBytes"] -= 1
        self.budget.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "exceeds reviewed"):
            self.run_gate()
        value["unknown"] = True
        self.budget.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "invalid reviewed"):
            self.run_gate()
        self.budget.write_text('{"schemaVersion":1,"schemaVersion":1}')
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.run_gate()

    def test_size_budget_schema_rejects_bool_and_unbound_evidence(self):
        valid = {"schemaVersion": 1, "maximumCompressedBytes": 100,
                 "referenceImageSha256": "a" * 64, "footprintReportSha256": "b" * 64}
        for key, bad in (("maximumCompressedBytes", True), ("maximumCompressedBytes", 2**31),
                         ("referenceImageSha256", ""), ("footprintReportSha256", "A" * 64)):
            with self.assertRaises(ValueError):
                module.validate_budget({**valid, key: bad})

    def test_publication_rejects_review_candidates_and_wrong_image_bytes(self):
        self.run_gate(review=True)
        self.budget.write_bytes(self.report.with_suffix(".budget-proposal.json").read_bytes())
        command = [sys.executable, str(ROOT / "guest/image/verify-release-size.py"),
                   str(self.candidate), str(self.report), str(self.budget)]
        failure = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(failure.returncode, 0)
        self.assertIn("review-only", failure.stderr)
        self.run_gate()
        subprocess.run(command, check=True)
        with self.candidate.open("r+b") as output:
            output.write(b"changed")
        failure = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(failure.returncode, 0)
        self.assertIn("do not match", failure.stderr)

    def test_guestfish_trailing_newline_produces_publishable_inventory(self):
        # guestfish prints its own newline after dpkg-query's final newline.
        self.packages.write_text("docker.io\t28.0\t12345\nrunc\t1.3.4\t34734\n\n")
        self.run_gate(review=True)
        expected = ["docker.io\t28.0\t12345", "runc\t1.3.4\t34734"]
        report = json.loads(self.report.read_text())
        self.assertEqual(report["packagesBefore"], expected)
        self.assertEqual(report["packagesAfter"], expected)
        variations.verify_inventory(self.packages, report["packagesAfter"])
        changed = self.root / "changed-packages.tsv"
        changed.write_text("docker.io\t29.0\t12345\nrunc\t1.3.4\t34734\n\n")
        with self.assertRaisesRegex(ValueError, "changed the pinned"):
            variations.verify_inventory(changed, report["packagesAfter"])
        self.budget.write_bytes(self.report.with_suffix(".budget-proposal.json").read_bytes())
        self.run_gate()
        subprocess.run([sys.executable, str(ROOT / "guest/image/verify-release-size.py"),
                        str(self.candidate), str(self.report), str(self.budget)], check=True)

    def test_invalid_or_empty_inventory_is_rejected_before_report_publication(self):
        for value in ("", "\n\n", "docker.io\t28.0\t12345\n\nrunc\t1.3.4\t34734\n",
                      "docker.io\t28.0\n", "docker.io\t28.0\t-1\n",
                      "docker.io\t\t12345\n", "docker.io\t28.0\t12345\ndocker.io\t28.0\t12345\n"):
            with self.subTest(value=value):
                self.packages.write_text(value)
                with self.assertRaisesRegex(ValueError, "package inventory"):
                    self.run_gate(review=True)
                self.assertFalse(self.report.exists())

    def test_gpt_crc_and_virtual_size_validation(self):
        raw = self.root / "raw"
        self.resize(raw, 8 * 1024**3)
        table = bytearray(128 * 128)
        table[0] = 1
        mbr = bytearray(512)
        mbr[450] = 0xEE
        mbr[510:512] = b"\x55\xaa"
        header = bytearray(512)
        header[:8] = b"EFI PART"
        struct.pack_into("<II", header, 8, 0x10000, 92)
        struct.pack_into("<QIII", header, 72, 2, 128, 128, zlib.crc32(table))
        struct.pack_into("<I", header, 16, zlib.crc32(header[:92]))
        with raw.open("r+b") as output:
            output.write(mbr + header + table)
        command = [sys.executable, str(ROOT / "guest/image/verify-raw.py"), str(raw)]
        subprocess.run(command, check=True)
        with raw.open("r+b") as output:
            output.seek(1024)
            output.write(b"\x02")
        failure = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(failure.returncode, 0)
        self.assertIn("partition table CRC", failure.stderr)


if __name__ == "__main__":
    unittest.main()
