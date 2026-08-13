#!/bin/bash
# Managed uninstall must require an exact confirmation and preserve every
# foreign or unsafe path. No VM is started by this test.
set -euo pipefail

HAMN=${HAMN:-build/hamn}
INSTALL=scripts/install-host.sh
WORK=$(mktemp -d /tmp/hamn-uninstall.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

prepare_install() {
    local name=$1
    TEST_HOME=$WORK/$name-home
    TEST_BINDIR=$WORK/$name-bin
    TEST_DATADIR=$WORK/$name-share/hamn/src
    mkdir -p "$TEST_HOME/.hamn/default" "$TEST_HOME/.hamn/cache"
    truncate -s 4096 "$TEST_HOME/.hamn/default/disk.img"
    printf '%s\n' cached-image >"$TEST_HOME/.hamn/cache/image"
    bash "$INSTALL" "$HAMN" "$TEST_BINDIR" "$TEST_DATADIR" \
        >"$WORK/$name-install.out"
}

prepare_install managed
HOME_DIR=$TEST_HOME
BINDIR=$TEST_BINDIR
DATADIR=$TEST_DATADIR
INSTALLED=$BINDIR/hamn
CANON_DATA_PARENT=$(cd "$(dirname "$DATADIR")" && pwd -P)
CANON_DATADIR=$CANON_DATA_PARENT/$(basename "$DATADIR")
DATA_LOCK=$(dirname "$DATADIR")/.$(basename "$DATADIR").hamn-install.lock

# n, uppercase Y, and EOF must preserve both runtime data and install files.
for answer in n Y; do
    if printf '%s\n' "$answer" | HOME="$HOME_DIR" "$INSTALLED" uninstall \
        >"$WORK/$answer.out" 2>"$WORK/$answer.err"; then
        echo "FAIL: uninstall accepted '$answer'" >&2
        exit 1
    fi
    [ -d "$HOME_DIR/.hamn" ] && [ -L "$INSTALLED" ] && [ -d "$DATADIR" ] || {
        echo "FAIL: uninstall changed data after '$answer'" >&2
        exit 1
    }
done
if HOME="$HOME_DIR" "$INSTALLED" uninstall \
    >"$WORK/eof.out" 2>"$WORK/eof.err" </dev/null; then
    echo "FAIL: uninstall accepted EOF" >&2
    exit 1
fi
[ -d "$HOME_DIR/.hamn" ] && [ -L "$INSTALLED" ] && [ -d "$DATADIR" ]
grep -Fq "managed installation root: $CANON_DATADIR" "$WORK/n.out" || {
    sed -n '1,160p' "$WORK/n.out" >&2
    sed -n '1,160p' "$WORK/n.err" >&2
    exit 1
}
grep -Fq "managed executable link: $INSTALLED" "$WORK/n.out"
grep -Fq "Hamn runtime root: $HOME_DIR/.hamn" "$WORK/n.out"
grep -Fq 'profile default (VM, Docker, and Kubernetes data)' "$WORK/n.out"
grep -Fq 'image cache:' "$WORK/n.out"

# An exact lower-case y removes only the proven installer and Hamn runtime.
printf 'y\n' | HOME="$HOME_DIR" "$INSTALLED" uninstall \
    >"$WORK/y.out" 2>"$WORK/y.err"
[ ! -e "$HOME_DIR/.hamn" ] && [ ! -L "$INSTALLED" ] && [ ! -e "$DATADIR" ] || {
    echo "FAIL: confirmed uninstall left managed files behind" >&2
    exit 1
}
[ ! -e "$BINDIR/.hamn-install.lock" ] && [ ! -e "$DATA_LOCK" ] || {
    echo "FAIL: confirmed uninstall left managed install locks behind" >&2
    exit 1
}
grep -Fq 'Hamn has been uninstalled.' "$WORK/y.out"

# A symlinked runtime root is never followed, even after a valid confirmation.
prepare_install unsafe
UNSAFE_HOME=$TEST_HOME
UNSAFE_BIN=$TEST_BINDIR
UNSAFE_DATA=$TEST_DATADIR
VICTIM=$WORK/victim
mkdir -p "$VICTIM"
printf '%s\n' keep >"$VICTIM/keep"
rm -rf "$UNSAFE_HOME/.hamn"
ln -s "$VICTIM" "$UNSAFE_HOME/.hamn"
if printf 'y\n' | HOME="$UNSAFE_HOME" "$UNSAFE_BIN/hamn" uninstall \
    >"$WORK/unsafe.out" 2>"$WORK/unsafe.err"; then
    echo "FAIL: uninstall accepted a symlinked runtime root" >&2
    exit 1
fi
grep -Fq 'refusing unsafe Hamn runtime path' "$WORK/unsafe.err"
grep -qx 'keep' "$VICTIM/keep"
[ -L "$UNSAFE_HOME/.hamn" ] && [ -L "$UNSAFE_BIN/hamn" ] && [ -d "$UNSAFE_DATA" ]

echo "PASS: uninstall confirmation and managed-path safety"
