#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/configure-rosetta.sh"
WORK=$(mktemp -d)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

BIN="$WORK/bin"
STATE="$WORK/binfmt-state"
MOUNTS="$WORK/mounts"
MOUNT_POINT="$WORK/mnt/rosetta"
LOG="$WORK/update-binfmts.log"
MOUNT_LOG="$WORK/mount.log"
mkdir -p "$BIN" "$MOUNT_POINT"
printf '%s\t%s\t%s\n' qemu-x86_64 enabled /usr/bin/qemu-x86_64-static >"$STATE"
: >"$MOUNTS"
: >"$LOG"
: >"$MOUNT_LOG"

cat >"$BIN/update-binfmts" <<'EOF'
#!/bin/bash
set -euo pipefail

state=${HAMN_ROSETTA_TEST_STATE:?}
log=${HAMN_ROSETTA_TEST_LOG:?}

entry_exists() {
    awk -F '\t' -v name="$1" '$1 == name { found = 1 } END { exit found ? 0 : 1 }' "$state"
}

set_entry() {
    local name=$1 status=$2 interpreter=$3 tmp
    tmp=$(mktemp "${state}.XXXXXX")
    awk -F '\t' -v name="$name" '$1 != name { print }' "$state" >"$tmp"
    printf '%s\t%s\t%s\n' "$name" "$status" "$interpreter" >>"$tmp"
    mv -f "$tmp" "$state"
}

case "${1:-}" in
--display)
    [ "$#" -eq 2 ] || exit 2
    awk -F '\t' -v name="$2" '$1 == name { print; found = 1 } END { exit found ? 0 : 1 }' "$state"
    ;;
--install)
    [ "$#" -ge 3 ] || exit 2
    printf '%s\n' "$*" >>"$log"
    set_entry "$2" enabled "$3"
    ;;
--remove)
    [ "$#" -eq 2 ] || exit 2
    entry_exists "$2" || exit 1
    tmp=$(mktemp "${state}.XXXXXX")
    awk -F '\t' -v name="$2" '$1 != name { print }' "$state" >"$tmp"
    mv -f "$tmp" "$state"
    ;;
--enable|--disable)
    [ "$#" -eq 2 ] || exit 2
    entry_exists "$2" || exit 1
    if [ "$1" = --disable ] && [ "${HAMN_FAIL_DISABLE_QEMU:-0}" = 1 ]; then
        exit 9
    fi
    status=enabled
    [ "$1" = --disable ] && status=disabled
    interpreter=$(awk -F '\t' -v name="$2" '$1 == name { print $3 }' "$state")
    set_entry "$2" "$status" "$interpreter"
    ;;
*) exit 2 ;;
esac
EOF

cat >"$BIN/mount" <<'EOF'
#!/bin/bash
set -euo pipefail

[ "$#" -eq 4 ] && [ "$1" = -t ] && [ "$2" = virtiofs ] || exit 2
printf '%s\n' "$*" >>"$HAMN_ROSETTA_TEST_MOUNT_LOG"
printf '%s %s virtiofs rw 0 0\n' "$3" "$4" >>"$HAMN_ROSETTA_TEST_MOUNTS"
EOF
chmod 0755 "$BIN/update-binfmts" "$BIN/mount"

run_rosetta() {
    HAMN_UPDATE_BINFMT="$BIN/update-binfmts" \
    HAMN_MOUNT="$BIN/mount" \
    HAMN_PROC_MOUNTS="$MOUNTS" \
    HAMN_ROSETTA_TAG=rosetta \
    HAMN_ROSETTA_MOUNT_POINT="$MOUNT_POINT" \
    HAMN_ROSETTA_PRESERVE=no \
    HAMN_ROSETTA_TEST_STATE="$STATE" \
    HAMN_ROSETTA_TEST_LOG="$LOG" \
    HAMN_ROSETTA_TEST_MOUNTS="$MOUNTS" \
    HAMN_ROSETTA_TEST_MOUNT_LOG="$MOUNT_LOG" \
        bash "$SCRIPT" "$@"
}

assert_entry() {
    local name=$1 status=$2 interpreter=$3
    grep -Fxq "${name}"$'\t'"${status}"$'\t'"${interpreter}" "$STATE"
}

printf '%s\n' '#!/bin/sh' 'exit 0' >"$MOUNT_POINT/rosetta"
chmod 0755 "$MOUNT_POINT/rosetta"

run_rosetta enable >"$WORK/enable.out"
grep -Fq 'Rosetta x86_64 translation is enabled' "$WORK/enable.out"
assert_entry hamn-rosetta enabled "$MOUNT_POINT/rosetta"
assert_entry qemu-x86_64 disabled /usr/bin/qemu-x86_64-static
grep -Fxq -- "-t virtiofs rosetta $MOUNT_POINT" "$MOUNT_LOG"
grep -Fq -- '--credentials yes --preserve no --fix-binary yes' "$LOG"

# Re-enabling replaces its own handler but never accumulates duplicate entries.
run_rosetta enable >"$WORK/enable-again.out"
test "$(awk -F '\t' '$1 == "hamn-rosetta" { count++ } END { print count + 0 }' "$STATE")" -eq 1

run_rosetta disable >"$WORK/disable.out"
grep -Fq 'qemu x86_64 translation is enabled' "$WORK/disable.out"
assert_entry qemu-x86_64 enabled /usr/bin/qemu-x86_64-static
if grep -Fq $'hamn-rosetta\t' "$STATE"; then
    echo "FAIL: disable retained the Rosetta binfmt handler" >&2
    exit 1
fi

# A missing runtime fails before changing the working qemu handler.
rm "$MOUNT_POINT/rosetta"
if run_rosetta enable >"$WORK/missing.out" 2>"$WORK/missing.err"; then
    echo "FAIL: enable accepted a missing Rosetta runtime" >&2
    exit 1
fi
grep -Fq 'Rosetta runtime is unavailable' "$WORK/missing.err"
assert_entry qemu-x86_64 enabled /usr/bin/qemu-x86_64-static

printf '%s\n' '#!/bin/sh' 'exit 0' >"$MOUNT_POINT/rosetta"
chmod 0755 "$MOUNT_POINT/rosetta"
if HAMN_FAIL_DISABLE_QEMU=1 run_rosetta enable >"$WORK/fail.out" \
    2>"$WORK/fail.err"; then
    echo "FAIL: enable accepted a qemu disable failure" >&2
    exit 1
fi
grep -Fq 'cannot disable qemu while Rosetta is active' "$WORK/fail.err"
assert_entry qemu-x86_64 enabled /usr/bin/qemu-x86_64-static
if grep -Fq $'hamn-rosetta\t' "$STATE"; then
    echo "FAIL: qemu disable failure retained a competing Rosetta handler" >&2
    exit 1
fi

echo "PASS: Rosetta binfmt registration is opt-in and restores qemu safely"
