#!/usr/bin/env python3
"""Record real same-build bytes and enforce the reviewed compressed-size budget.

--review-only emits a proposal, never a reviewed budget. Distribution builds
require release-size-budget.json committed after reviewing actual image/runtime
reports. No default or synthetic byte limit substitutes for that first result.
"""
import argparse
import hashlib
import json
import pathlib
import re


def sha256(path):
    with path.open("rb") as source:
        digest = hashlib.sha256()
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
        return digest.hexdigest()


def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise ValueError("duplicate key: " + key)
        result[key] = value
    return result


def validate_budget(value):
    if (not isinstance(value, dict) or set(value) != {
            "schemaVersion", "maximumCompressedBytes", "referenceImageSha256",
            "footprintReportSha256"} or type(value["schemaVersion"]) is not int or value["schemaVersion"] != 1 or
            type(value["maximumCompressedBytes"]) is not int or
            not 0 < value["maximumCompressedBytes"] < 2**31 or
            any(not isinstance(value[key], str) or
                re.fullmatch(r"[0-9a-f]{64}", value[key]) is None
                for key in ("referenceImageSha256", "footprintReportSha256"))):
        raise ValueError("invalid reviewed size budget")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("baseline", "candidate", "packages-before", "packages-after", "report", "budget"):
        parser.add_argument("--" + name, type=pathlib.Path, required=True)
    parser.add_argument("--base-sha256", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--review-only", action="store_true")
    args = parser.parse_args()
    if (re.fullmatch(r"[0-9a-f]{64}", args.base_sha256) is None or
            re.fullmatch(r"[0-9a-f]{40}", args.source_revision) is None):
        raise ValueError("invalid source identity")
    baseline = args.baseline.stat().st_size
    candidate = args.candidate.stat().st_size
    if baseline <= 0 or candidate <= 0:
        raise ValueError("empty baseline or candidate")
    required = max(64 * 1024**2, (baseline + 19) // 20)
    report = {
        "schemaVersion": 1, "baseImageSha256": args.base_sha256,
        "sourceRevision": args.source_revision, "virtualBytes": 8 * 1024**3,
        "baselineCompressedBytes": baseline, "compressedBytes": candidate,
        "baselineSha256": sha256(args.baseline),
        "imageSha256": sha256(args.candidate),
        "savedBytes": baseline - candidate, "requiredSavingsBytes": required,
        "packagesBefore": args.packages_before.read_text().splitlines(),
        "packagesAfter": args.packages_after.read_text().splitlines(),
        "cleanup": ["build dependencies", "apt archives and lists", "temporary sources",
                    "logs and journals", "cloud-init state", "machine-id", "SSH host keys",
                    "systemd random seed", "free filesystem blocks"],
        "runtimeValidation": "required: physical boot, Docker, Compose, Buildx, binfmt, Rosetta, reboot",
        "reviewOnly": args.review_only,
    }
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    if candidate >= 2**31:
        raise ValueError("compressed guest image exceeds the GitHub release asset limit")
    if baseline - candidate < required:
        raise ValueError("image savings are below max(64 MiB, 5%)")
    if args.review_only:
        proposal = {
            "schemaVersion": 1, "maximumCompressedBytes": candidate,
            "referenceImageSha256": report["imageSha256"],
            "footprintReportSha256": sha256(args.report),
        }
        args.report.with_suffix(".budget-proposal.json").write_text(json.dumps(proposal, indent=2) + "\n")
    else:
        if not args.budget.is_file() or args.budget.is_symlink():
            raise ValueError("reviewed release-size-budget.json is required; use review-only to produce evidence")
        budget = json.loads(args.budget.read_text(), object_pairs_hook=pairs)
        validate_budget(budget)
        if candidate > budget["maximumCompressedBytes"]:
            raise ValueError("guest image exceeds reviewed size budget; review the footprint report before changing it")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        raise SystemExit("hamn guest image size gate: " + str(error))
