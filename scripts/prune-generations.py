#!/usr/bin/env python3
"""Collect inactive managed generations while install-transaction locks are held.

Keep active, immediate predecessor, caller source, open files, and recovery roots.
Unknown ownership, incomplete copies and an unavailable process scan fail closed.
Retire by rename; marker-last deletion makes an interrupted sweep retryable.
This helper never touches profiles, images, legacy source or external commands.
"""
import fcntl
import hashlib
import os
from pathlib import Path
import re
import stat
import subprocess
import sys


def owned(path, directory=False, mode=None):
    s = path.lstat()
    return (s.st_uid == os.getuid()
            and (stat.S_ISDIR(s.st_mode) if directory else stat.S_ISREG(s.st_mode))
            and not s.st_mode & 0o022
            and (directory or s.st_nlink == 1)
            and (mode is None or stat.S_IMODE(s.st_mode) == mode))


def tree_owned(path):
    if not owned(path, directory=True):
        return False
    return all(tree_owned(p) if p.is_dir() and not p.is_symlink() else owned(p)
               for p in path.iterdir())


def erase(path):
    # Preserve the ownership receipt until every payload entry is gone.
    for p in path.iterdir():
        if p.name == '.hamn-generation':
            continue
        if p.is_dir():
            erase(p)
        else:
            p.unlink()
    marker = path / '.hamn-generation'
    if marker.exists():
        marker.unlink()
    path.rmdir()


def pending(cache):
    # exists()/glob() can suppress permission errors in newer Python releases.
    # Only ENOENT proves absence; unreadable recovery state must retain history.
    try:
        cache.lstat()
    except FileNotFoundError:
        return False
    if not owned(cache, directory=True):
        return True
    with os.scandir(cache) as entries:
        return any(entry.name.startswith('.hamn-update-') for entry in entries)


def recovery_pending(path):
    value = path.read_text()
    if (not value.startswith('/') or '\n' in value
            or path.name != '.hamn-recovery-root-' + hashlib.sha256(value.encode()).hexdigest()):
        return True
    return pending(Path(value))


def generation_target(path, root):
    return (path.name == 'hamn' and path.parent.name == 'bin'
            and path.parent.parent.parent == root
            and re.fullmatch(r'[0-9a-f]{64}-[A-Za-z0-9]{6}', path.parent.parent.name))


def collect(bindir, datadir, previous, source):
    locks = sorted([bindir / '.hamn-transaction.lock',
                    datadir.parent / ('.' + datadir.name + '.hamn-transaction.lock')])
    for fd, lock in zip((6, 7), locks):
        a, b = lock.lstat(), os.fstat(fd)
        if not owned(lock, mode=0o600) or (a.st_dev, a.st_ino) != (b.st_dev, b.st_ino):
            raise ValueError('collection requires the install transaction locks')
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    root = datadir / '.hamn-generations'
    if not owned(datadir, True, 0o755) or not owned(root, True, 0o755):
        raise ValueError('unsafe generation root')
    if any(ord(c) < 32 or ord(c) > 126 or c == '\\' for c in str(root)):
        raise ValueError('generation path cannot be matched safely in process output')
    if pending(Path.home() / '.hamn/cache'):
        print('hamn: generation cleanup deferred while recovery metadata exists', file=sys.stderr)
        return
    active = Path(os.readlink(bindir / 'hamn'))
    if not generation_target(active, root):
        raise ValueError('active link is outside the managed generation root')
    keep = [active, Path(previous), Path(source)]
    predecessor = active.parent.parent / '.hamn-previous-target'
    if predecessor.exists() or predecessor.is_symlink():
        if not owned(predecessor, mode=0o600):
            raise ValueError('unsafe predecessor reference')
        value = predecessor.read_text()
        target = Path(value.rstrip('\n'))
        if (not value.endswith('\n') or value.count('\n') != 1
                or not generation_target(target, root)):
            raise ValueError('invalid predecessor reference')
        keep.append(target)
    # Query the complete open-file table once. A failed/partial scan cannot
    # establish that an old executable or its support files are unused.
    scan = subprocess.run(['/usr/sbin/lsof', '-nP', '-F', 'n'],
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=30)
    if scan.returncode or scan.stderr:
        raise ValueError('open-file scan unavailable or incomplete')
    opened = [os.fsdecode(line[1:]) for line in scan.stdout.splitlines()
              if line.startswith(b'n/')]
    # Older update scripts predate the shared locks. Defer while any observed
    # updater has its script open without a transaction lock descriptor.
    process_files = []
    for line in scan.stdout.splitlines() + [b'p']:
        if line.startswith(b'p'):
            if (any(p.endswith('/scripts/update-host.sh') for p in process_files)
                    and not any(p.endswith('.hamn-transaction.lock') for p in process_files)):
                raise ValueError('legacy updater is still running')
            process_files = []
        elif line.startswith(b'n/'):
            process_files.append(os.fsdecode(line[1:]))
    ids = [hashlib.sha256((str(p) + '\0').encode()).hexdigest()
           for p in (bindir, datadir)]
    for path in sorted(root.iterdir()):
        retired = path.name.startswith('.retired-')
        name = path.name.removeprefix('.retired-')
        if not re.fullmatch(r'[0-9a-f]{64}-[A-Za-z0-9]{6}', name):
            continue
        try:
            if not tree_owned(path):
                continue
            if any(p == path or path in p.parents for p in keep):
                continue
            if any(p == str(path) or p.startswith(str(path) + '/') for p in opened):
                continue
            marker = path / '.hamn-generation'
            if retired and not any(path.iterdir()):
                path.rmdir()
                continue
            expected = ('version=1\nbinary_sha256=' + name[:64]
                        + '\nbindir_id=' + ids[0] + '\ndatadir_id=' + ids[1] + '\n')
            if not marker.exists() or not owned(marker, mode=0o600) or marker.read_text() != expected:
                continue
            if any(recovery_pending(p) for p in path.glob('.hamn-recovery-root-*')):
                continue
            if not retired:
                retention = path / '.hamn-retention'
                if (not retention.exists() or not owned(retention, mode=0o600)
                        or retention.read_text() != 'version=1\n'):
                    continue
                binary = path / 'bin/hamn'
                if not owned(binary, mode=0o755) or hashlib.sha256(binary.read_bytes()).hexdigest() != name[:64]:
                    continue
                destination = root / ('.retired-' + name)
                if destination.exists() or destination.is_symlink():
                    continue
                path.rename(destination)
                path = destination
            erase(path)
            print('hamn: removed obsolete generation ' + name)
        except (OSError, ValueError) as error:
            print('hamn: generation cleanup deferred: ' + str(error), file=sys.stderr)


if __name__ == '__main__':
    try:
        collect(Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], sys.argv[4])
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print('hamn: generation cleanup deferred: ' + str(error), file=sys.stderr)
        sys.exit(1)
