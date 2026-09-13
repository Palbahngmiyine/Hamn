#!/bin/bash
# Sourced by install/update with BINDIR and DATADIR. Hold both root locks until
# process exit, including recovery and generation collection. Children inherit
# descriptors, not permission to bypass locks; every use checks their identity.
if [ -L "$BINDIR" ] || [ -L "$DATADIR" ]; then
    echo "hamn: refusing symlinked transaction root" >&2
    exit 1
fi
mkdir -p "$BINDIR" "$(dirname "$DATADIR")"
BINDIR=$(cd "$BINDIR" && pwd -P)
DATADIR=$(cd "$(dirname "$DATADIR")" && pwd -P)/$(basename "$DATADIR")
transaction_lock() {
    local path=$1 fd=$2
    python3 - "$path" <<'PY'
import os, stat, sys
p = sys.argv[1]
s = os.lstat(os.path.dirname(p))
if not stat.S_ISDIR(s.st_mode) or s.st_uid != os.getuid() or stat.S_IMODE(s.st_mode) not in (0o700, 0o755):
    sys.exit('hamn: unsafe transaction lock parent')
try:
    f = os.open(p, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(f)
except FileExistsError:
    pass
s = os.lstat(p)
if not stat.S_ISREG(s.st_mode) or s.st_uid != os.getuid() or stat.S_IMODE(s.st_mode) != 0o600 or s.st_nlink != 1:
    sys.exit('hamn: unsafe transaction lock')
PY
    # A nested installer can reuse an inherited descriptor only for this inode.
    if ! python3 - "$path" "$fd" <<'PY'
import os, sys
try:
    a, b = os.lstat(sys.argv[1]), os.fstat(int(sys.argv[2]))
    assert (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino)
except (OSError, AssertionError):
    sys.exit(1)
PY
    then
        if [ "$fd" = 6 ]; then exec 6>>"$path"; else exec 7>>"$path"; fi
    fi
    python3 - "$path" "$fd" <<'PY'
import fcntl, os, stat, sys
p, fd = sys.argv[1], int(sys.argv[2])
fcntl.flock(fd, fcntl.LOCK_EX)
a, b = os.lstat(p), os.fstat(fd)
if (a.st_dev, a.st_ino) != (b.st_dev, b.st_ino) or not stat.S_ISREG(a.st_mode) or a.st_uid != os.getuid() or stat.S_IMODE(a.st_mode) != 0o600 or a.st_nlink != 1:
    sys.exit('hamn: transaction lock changed')
PY
}
transaction_one=$BINDIR/.hamn-transaction.lock
transaction_two=$(dirname "$DATADIR")/.$(basename "$DATADIR").hamn-transaction.lock
if [[ "$transaction_two" < "$transaction_one" ]]; then
    transaction_swap=$transaction_one
    transaction_one=$transaction_two
    transaction_two=$transaction_swap
fi
transaction_lock "$transaction_one" 6
transaction_lock "$transaction_two" 7
