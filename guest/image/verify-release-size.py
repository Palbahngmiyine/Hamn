#!/usr/bin/env python3
"""Fail-closed publication boundary for an actual image and its size evidence."""
import importlib.util
import json
import pathlib
import re
import sys

spec = importlib.util.spec_from_file_location("size_contract", pathlib.Path(__file__).with_name("verify-size.py"))
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)


def validate(image, report_path, budget_path):
    for path in (image, report_path, budget_path):
        if not path.is_file() or path.is_symlink():
            raise ValueError("regular image, size report and reviewed release-size-budget.json are required")
    report = json.loads(report_path.read_text(), object_pairs_hook=contract.pairs)
    budget = json.loads(budget_path.read_text(), object_pairs_hook=contract.pairs)
    contract.validate_budget(budget)
    if (not isinstance(report, dict) or type(report.get("schemaVersion")) is not int or
            report.get("schemaVersion") != 1 or report.get("reviewOnly") is not False):
        raise ValueError("review-only or invalid image report cannot be published")
    for key in ("compressedBytes", "baselineCompressedBytes", "savedBytes", "requiredSavingsBytes", "virtualBytes"):
        if type(report.get(key)) is not int or report[key] <= 0:
            raise ValueError("invalid image report sizes")
    for key in ("imageSha256", "baselineSha256", "baseImageSha256"):
        if not isinstance(report.get(key), str) or re.fullmatch(r"[0-9a-f]{64}", report[key]) is None:
            raise ValueError("invalid image report digest")
    if not isinstance(report.get("sourceRevision"), str) or re.fullmatch(r"[0-9a-f]{40}", report["sourceRevision"]) is None:
        raise ValueError("invalid image source revision")
    actual_size = image.stat().st_size
    baseline = report["baselineCompressedBytes"]
    required = max(64 * 1024**2, (baseline + 19) // 20)
    if (report["virtualBytes"] != 8 * 1024**3 or report["compressedBytes"] != actual_size or
            actual_size >= 2**31 or actual_size > budget["maximumCompressedBytes"] or
            report["savedBytes"] != baseline - actual_size or baseline - actual_size < required or
            report["requiredSavingsBytes"] != required or contract.sha256(image) != report["imageSha256"]):
        raise ValueError("image bytes, savings or reviewed budget do not match size evidence")
    for key in ("packagesBefore", "packagesAfter", "cleanup"):
        if not isinstance(report.get(key), list) or not report[key] or any(not isinstance(item, str) or not item for item in report[key]):
            raise ValueError("image footprint evidence is missing")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: verify-release-size.py IMAGE SIZE_REPORT REVIEWED_BUDGET")
    try:
        validate(*(pathlib.Path(value) for value in sys.argv[1:]))
    except (OSError, ValueError) as error:
        raise SystemExit("hamn guest image release size gate: " + str(error))
