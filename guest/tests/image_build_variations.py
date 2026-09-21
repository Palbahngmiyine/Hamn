#!/usr/bin/env python3
"""Generate real cleanup variations from one exported provisioned baseline.

Requires Linux arm64, libguestfs and the normal builder dependencies. This is
an explicitly invoked integration fixture, not a portable synthetic test. It
never changes the supplied image or boots a VM. Its output index deliberately
marks physical runtime validation pending for every exact image digest.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import random
import shutil
import signal
import stat
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "guest/image"))
from image_evidence import regular

size_spec = importlib.util.spec_from_file_location("image_size", ROOT / "guest/image/verify-size.py")
size_contract = importlib.util.module_from_spec(size_spec)
size_spec.loader.exec_module(size_contract)


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def variations(seed, count):
    if not 0 <= seed < 2**32 or not 1 <= count <= 4:
        raise ValueError("seed must be uint32 and case count must be between one and four")
    rng = random.Random(seed)
    return [{"case": index, "seed": rng.randrange(2**32),
             "regenerableBytesPerFile": 0 if index == 0 else rng.randrange(1024, 65537)}
            for index in range(count)]


def verify_inventory(path, expected):
    if size_contract.package_inventory(path) != expected:
        raise ValueError("generated cleanup changed the pinned post-cleanup package inventory")


def injection_script(case):
    # Fixed guest-only paths: generated input is data, never a shell fragment.
    return f'''import pathlib, random
rng = random.Random({case["seed"]})
for name in ("/var/log/hamn-generated.log", "/var/cache/apt/archives/hamn-generated.deb",
             "/tmp/hamn-generated", "/var/tmp/hamn-generated"):
    path = pathlib.Path(name)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(rng.randbytes({case["regenerableBytesPerFile"]}))
pathlib.Path("/etc/machine-id").write_text(rng.randbytes(16).hex() + "\\n")
pathlib.Path("/etc/ssh/ssh_host_generated_key").write_bytes(rng.randbytes(64))
'''


def run(command, *, log, timeout=1200, capture=False):
    with log.open("ab") as output:
        output.write((json.dumps([str(item) for item in command]) + "\n").encode())
        output.flush()
        child = subprocess.Popen(command, stdout=subprocess.PIPE if capture else output,
                                 stderr=output, start_new_session=True)
        try:
            data, _ = child.communicate(timeout=timeout)
        except BaseException:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.communicate(timeout=5)
            raise
        if child.returncode:
            raise subprocess.CalledProcessError(child.returncode, command)
        if data is not None:
            output.write(data)
        return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--size-report", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=20260921)
    parser.add_argument("--case-count", type=int, default=2)
    args = parser.parse_args()
    cases = variations(args.seed, args.case_count)
    if platform.system() != "Linux" or platform.machine() not in ("aarch64", "arm64"):
        raise ValueError("real image variations require Linux arm64")
    for command in ("qemu-img", "virt-customize", "guestfish", "cc", "git"):
        if shutil.which(command) is None:
            raise ValueError("missing image builder dependency: " + command)
    baseline_info = regular(args.baseline)
    regular(args.size_report)
    report = json.loads(args.size_report.read_text())
    baseline_hash = digest(args.baseline)
    if (baseline_info.st_size != report["baselineCompressedBytes"]
            or baseline_hash != report["baselineSha256"]):
        raise ValueError("exported baseline does not match the actual build report")
    output = args.output_directory
    # Exclusive directory ownership prevents overwriting previous evidence.
    parent = output.parent.lstat()
    if (not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.geteuid()
            or parent.st_mode & 0o022):
        raise ValueError("unsafe variation output parent")
    output.mkdir(mode=0o700)
    evidence = {"schemaVersion": 1, "seed": args.seed, "baselineSha256": baseline_hash,
                "baselineSizeReportSha256": digest(args.size_report),
                "sourceRevision": subprocess.check_output(["git", "-C", ROOT, "rev-parse", "HEAD"], text=True, timeout=5).strip(),
                "physicalRuntimeValidation": "pending", "variants": []}
    with tempfile.TemporaryDirectory(prefix=".variation-stage-", dir=output) as temporary:
        work = Path(temporary)
        decoder = work / "extract-check"
        build_log = output / "decoder-build.log"
        run(["cc", "-D_GNU_SOURCE", "-std=c11", "-O2", "-Wall", "-Wextra",
             "-Werror=implicit-function-declaration", "-I" + str(ROOT / "host"),
             ROOT / "guest/image/extract-check.c", ROOT / "host/image/qcow2.c", "-lz", "-o", decoder], log=build_log)
        before = work / "packages-before.tsv"
        before.write_text("\n".join(report["packagesBefore"]) + "\n")
        for case in cases:
            name = "case-" + str(case["case"])
            log = output / (name + ".log")
            stage, compact = work / "stage.img", work / "compact.img"
            run(["qemu-img", "convert", "-q", "-f", "qcow2", "-O", "qcow2", args.baseline, stage], log=log)
            injection = work / "inject.py"
            injection.write_text(injection_script(case))
            run(["virt-customize", "-a", stage, "--upload", str(injection) + ":/root/hamn-inject.py",
                 "--run-command", "python3 /root/hamn-inject.py && rm /root/hamn-inject.py",
                 "--upload", str(ROOT / "guest/image/slim-guest.sh") + ":/root/hamn-slim.sh",
                 "--run-command", "bash /root/hamn-slim.sh && rm /root/hamn-slim.sh"], log=log)
            run(["guestfish", "--rw", "add-drive", stage, "format:qcow2", "discard:enable", ":", "run",
                 ":", "mount", "/dev/sda3", "/", ":", "fstrim", "/", ":", "umount-all", ":", "shutdown"], log=log)
            after = work / "packages-after.tsv"
            packages = run(["guestfish", "--ro", "--format=qcow2", "-a", stage, "-i",
                "command", "dpkg-query -W -f=${Package}\\t${Version}\\t${Installed-Size}\\n"], log=log, timeout=300, capture=True)
            after.write_bytes(packages)
            verify_inventory(after, report["packagesAfter"])
            run(["qemu-img", "convert", "-q", "-f", "qcow2", "-O", "qcow2", "-o", "compression_type=zlib", "-c", stage, compact], log=log)
            run(["qemu-img", "compare", "-q", "-f", "qcow2", "-F", "qcow2", stage, compact], log=log)
            raw, reference = work / "extracted.raw", work / "reference.raw"
            run([decoder, compact, raw], log=log)
            run(["qemu-img", "convert", "-q", "-f", "qcow2", "-O", "raw", compact, reference], log=log)
            raw_hash = digest(raw)
            if raw_hash != digest(reference):
                raise ValueError("generated image decoder differs from qemu-img")
            run([sys.executable, ROOT / "guest/image/verify-raw.py", raw], log=log)
            size_report = output / (name + ".size-report.json")
            run([sys.executable, ROOT / "guest/image/verify-size.py", "--baseline", args.baseline,
                 "--candidate", compact, "--packages-before", before, "--packages-after", after,
                 "--report", size_report, "--budget", ROOT / "guest/image/release-size-budget.json",
                 "--base-sha256", report["baseImageSha256"], "--source-revision", report["sourceRevision"],
                 "--review-only"], log=log)
            target = output / (name + ".img")
            os.link(compact, target)
            compact.unlink()
            evidence["variants"].append({**case, "image": target.name,
                "imageSha256": digest(target), "rawSha256": raw_hash,
                "sizeReport": size_report.name, "structuralChecks": "passed",
                "physicalRuntimeValidation": "pending"})
            for path in (stage, raw, reference): path.unlink()
    if digest(args.baseline) != baseline_hash:
        raise ValueError("input baseline changed during variation generation")
    (output / "variations.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"Generated {len(cases)} structurally validated images; physical runtime validation remains pending: {output}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        raise SystemExit("hamn image variations: " + str(error))
