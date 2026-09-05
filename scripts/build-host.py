#!/usr/bin/env python3
"""Serialize host publication; only publish a signed, verified complete executable."""
import fcntl
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def publish(output, version, profile):
    output = Path(output).absolute()
    output.parent.mkdir(parents=True, exist_ok=True)
    lock = os.open(output.parent / '.hamn-publish.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX)
        env = dict(os.environ, HAMN_VERSION=version)
        subprocess.run(['cargo', 'build', '--locked', '--profile', profile], env=env, check=True)
        source = Path(os.environ.get('CARGO_TARGET_DIR', 'target')) / ('debug' if profile == 'dev' else profile) / 'hamn'
        descriptor, path = tempfile.mkstemp(prefix='.hamn-candidate-', dir=output.parent)
        os.close(descriptor)
        try:
            shutil.copyfile(source, path)
            os.chmod(path, 0o755)
            subprocess.run(['codesign', '--force', '--sign', '-', '--entitlements', 'host/entitlements.plist', path], check=True)
            subprocess.run(['bash', 'scripts/check-host-binary.sh', path], check=True)
            actual = subprocess.check_output([path, '--version'], text=True).strip()
            if actual != 'hamn ' + version:
                raise RuntimeError('linked executable version does not match requested version')
            with open(path, 'rb') as candidate:
                os.fsync(candidate.fileno())
            os.replace(path, output)
            directory = os.open(output.parent, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            if os.path.exists(path):
                os.unlink(path)
    finally:
        os.close(lock)


if __name__ == '__main__':
    publish(*sys.argv[1:])
