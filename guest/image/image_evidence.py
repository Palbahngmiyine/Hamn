#!/usr/bin/env python3
"""Export a verified same-build baseline without replacing existing evidence.

Only caller-owned regular single-link sources and non-writable owned output
directories are accepted. Files become visible only after full copy, hash and
fsync; exclusive links never overwrite a competing artifact. A failed publish
removes only links still naming this attempt's inode. These are review inputs,
not independently approved release artifacts.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile


def regular(path):
    info = Path(path).lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
            or info.st_nlink != 1 or info.st_mode & 0o022):
        raise ValueError("unsafe baseline evidence source")
    return info


def output_paths(destination, reserved=()):
    path = Path(destination)
    parent = path.parent
    info = parent.lstat()
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid()
            or info.st_mode & 0o022):
        raise ValueError("unsafe baseline output directory")
    if not path.name or any(ord(char) < 32 for char in path.name):
        raise ValueError("invalid baseline output name")
    paths = [parent.resolve() / path.name, parent.resolve() / (path.name + ".sha256")]
    reserved = {Path(item).parent.resolve() / Path(item).name for item in reserved}
    for target in paths:
        if os.path.lexists(target) or target in reserved:
            raise ValueError("baseline output collides with an existing or reserved artifact")
    return paths


def publish(source, destination, report):
    paths = output_paths(destination)
    source = Path(source)
    info = regular(source)
    regular(report)
    value = json.loads(Path(report).read_text())
    expected = value["baselineSha256"]
    if info.st_size <= 0 or info.st_size != value["baselineCompressedBytes"]:
        raise ValueError("baseline size differs from same-build report")
    parent = paths[0].parent
    published = []
    parent_fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        with tempfile.TemporaryDirectory(prefix=".hamn-baseline-", dir=parent) as temporary:
            stage = Path(temporary) / "image.img"
            checksum = Path(temporary) / "image.img.sha256"
            digest = hashlib.sha256()
            source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(source_fd, "rb") as input_file, stage.open("xb") as output:
                opened = os.fstat(input_file.fileno())
                if ((opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino)
                        or opened.st_nlink != 1 or opened.st_uid != os.geteuid()
                        or opened.st_mode & 0o022):
                    raise ValueError("baseline source changed while opening")
                os.fchmod(output.fileno(), 0o600)
                for block in iter(lambda: input_file.read(1024 * 1024), b""):
                    digest.update(block)
                    output.write(block)
                output.flush()
                os.fsync(output.fileno())
            if stage.stat().st_size != info.st_size or digest.hexdigest() != expected:
                raise ValueError("baseline digest differs from same-build report")
            with checksum.open("x") as output:
                os.fchmod(output.fileno(), 0o600)
                output.write(f"{expected}  {paths[0].name}\n")
                output.flush()
                os.fsync(output.fileno())
            for staged, target in ((checksum, paths[1]), (stage, paths[0])):
                # os.link is an atomic no-replace publication on this filesystem.
                os.link(staged, target.name, dst_dir_fd=parent_fd, follow_symlinks=False)
                published.append((target, staged.stat().st_ino))
            os.fsync(parent_fd)
    except BaseException:
        for target, inode in reversed(published):
            try:
                if target.lstat().st_ino == inode:
                    target.unlink()
            except FileNotFoundError:
                pass
        raise
    finally:
        os.close(parent_fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check")
    check.add_argument("destination")
    check.add_argument("reserved", nargs="*")
    export = commands.add_parser("publish")
    export.add_argument("source")
    export.add_argument("destination")
    export.add_argument("report")
    args = parser.parse_args()
    if args.command == "check":
        output_paths(args.destination, args.reserved)
    else:
        publish(args.source, args.destination, args.report)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise SystemExit("hamn baseline evidence: " + str(error))
