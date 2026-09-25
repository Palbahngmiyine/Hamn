#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SCRIPT=${HAMN_ROSETTA_SCRIPT:-"$ROOT/scripts/configure-rosetta.sh"}
WORK=$(mktemp -d)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

BIN="$WORK/bin"
STATE="$WORK/binfmt-state"
ROSETTA_STATE="$STATE.rosetta"
ROSETTA_DATABASE="$WORK/rosetta-database"
MOUNTS="$WORK/mounts"
MOUNT_POINT="$WORK/mnt/rosetta"
LOG="$WORK/update-binfmts.log"
MOUNT_LOG="$WORK/mount.log"
mkdir -p "$BIN" "$MOUNT_POINT"
printf '%s\t%s\t%s\n' qemu-x86_64 enabled /usr/bin/qemu-x86_64-static >"$STATE"
: >"$ROSETTA_STATE"
: >"$MOUNTS"
: >"$LOG"
: >"$MOUNT_LOG"

cat >"$BIN/update-binfmts" <<'EOF'
#!/bin/bash
set -euo pipefail

state=${HAMN_ROSETTA_TEST_STATE:?}
log=${HAMN_ROSETTA_TEST_LOG:?}
printf '%s\n' "$*" >>"$log"
if [ "${1:-}" = --admindir ]; then
    [ "$2" = "$HAMN_ROSETTA_BINFMT_DIR" ] && [ -d "$2" ] || exit 2
    state=$state.rosetta
    shift 2
fi

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
    awk -F '\t' -v name="$2" '$1 == name { printf "%s (%s):\n interpreter = %s\n", $1, $2, $3; found = 1 } END { exit found ? 0 : 1 }' "$state"
    ;;
--install)
    [ "$#" -ge 3 ] || exit 2
    if [ "${HAMN_FAIL_INSTALL_ROSETTA:-}" = early ]; then exit 9; fi
    set_entry "$2" enabled "$3"
    # Real update-binfmts scans the entire selected database, including
    # disabled entries, before it writes to the kernel's register file.
    if [ "$(wc -l <"$state")" -gt 1 ]; then
        set_entry "$2" disabled "$3"
        echo 'same magic with fix-binary in selected database' >&2
        exit 9
    fi
    if [ "${HAMN_FAIL_INSTALL_ROSETTA:-}" = partial ]; then exit 9; fi
    if [ "${HAMN_FAIL_INSTALL_ROSETTA:-}" = disabled ]; then set_entry "$2" disabled "$3"; fi
    ;;
--remove)
    [ "$#" -eq 3 ] || { echo '--remove needs <name> <path>' >&2; exit 2; }
    entry_exists "$2" || exit 1
    interpreter=$(awk -F '\t' -v name="$2" '$1 == name { print $3 }' "$state")
    [ "$3" = "$interpreter" ] || exit 2
    tmp=$(mktemp "${state}.XXXXXX")
    awk -F '\t' -v name="$2" '$1 != name { print }' "$state" >"$tmp"
    mv -f "$tmp" "$state"
    ;;
--enable|--disable)
    [ "$#" -eq 2 ] || exit 2
    entry_exists "$2" || exit 1
    if [ "$1" = --disable ] && [ "${HAMN_FAIL_DISABLE_QEMU:-0}" = 1 ]; then
        interpreter=$(awk -F '\t' -v name="$2" '$1 == name { print $3 }' "$state")
        set_entry "$2" disabled "$interpreter"
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
    HAMN_ROSETTA_BINFMT_DIR="$ROSETTA_DATABASE" \
    HAMN_ROSETTA_TEST_STATE="$STATE" \
    HAMN_ROSETTA_TEST_LOG="$LOG" \
    HAMN_ROSETTA_TEST_MOUNTS="$MOUNTS" \
    HAMN_ROSETTA_TEST_MOUNT_LOG="$MOUNT_LOG" \
        bash "$SCRIPT" "$@"
}

assert_entry() {
    local name=$1 status=$2 interpreter=$3 state=$STATE
    [ "$name" != hamn-rosetta ] || state=$ROSETTA_STATE
    grep -Fxq "${name}"$'\t'"${status}"$'\t'"${interpreter}" "$state"
}

assert_rosetta_removed() {
    if grep -Fq $'hamn-rosetta\t' "$STATE" "$ROSETTA_STATE"; then
        echo 'FAIL: Rosetta handler remains in a database' >&2
        exit 1
    fi
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
test "$(awk -F '\t' '$1 == "hamn-rosetta" { count++ } END { print count + 0 }' "$ROSETTA_STATE")" -eq 1

run_rosetta disable >"$WORK/disable.out"
grep -Fq 'qemu x86_64 translation is enabled' "$WORK/disable.out"
assert_entry qemu-x86_64 enabled /usr/bin/qemu-x86_64-static
assert_rosetta_removed

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
assert_rosetta_removed

# Registration errors can happen before or after persistent/kernel state is
# written, including when replacing an already enabled Rosetta handler.
for failure in early partial disabled; do
    run_rosetta enable >"$WORK/before-$failure.out"
    if HAMN_FAIL_INSTALL_ROSETTA=$failure run_rosetta enable \
        >"$WORK/$failure.out" 2>"$WORK/$failure.err"; then
        echo "FAIL: accepted registration failure: $failure" >&2
        exit 1
    fi
    assert_entry qemu-x86_64 enabled /usr/bin/qemu-x86_64-static
    assert_rosetta_removed
done

# The pre-release migration out of qemu's default database is gone: Rosetta
# changes only its own database and never touches another default entry.
printf '%s\t%s\t%s\n' hamn-rosetta disabled "$MOUNT_POINT/rosetta" >>"$STATE"
: >"$LOG"
run_rosetta disable >"$WORK/legacy-disable.out"
run_rosetta enable >"$WORK/legacy-enable.out"
assert_entry qemu-x86_64 disabled /usr/bin/qemu-x86_64-static
assert_entry hamn-rosetta enabled "$MOUNT_POINT/rosetta"
grep -Fxq "hamn-rosetta"$'\t'"disabled"$'\t'"$MOUNT_POINT/rosetta" "$STATE"
if grep -Eq '^--[a-z-]+ hamn-rosetta( |$)' "$LOG"; then
    echo 'FAIL: Rosetta configuration edited the default binfmt database' >&2
    exit 1
fi
run_rosetta disable >"$WORK/legacy-restore.out"
assert_entry qemu-x86_64 enabled /usr/bin/qemu-x86_64-static
awk -F '\t' '$1 != "hamn-rosetta" { print }' "$STATE" >"$STATE.clean"
mv "$STATE.clean" "$STATE"
assert_rosetta_removed

rm -rf "$ROSETTA_DATABASE"
ln -s "$WORK" "$ROSETTA_DATABASE"
if run_rosetta enable >"$WORK/symlink.out" 2>"$WORK/symlink.err"; then
    echo 'FAIL: accepted a symlinked Rosetta database' >&2
    exit 1
fi
grep -Fq 'Rosetta binfmt database is a symlink' "$WORK/symlink.err"
assert_entry qemu-x86_64 enabled /usr/bin/qemu-x86_64-static

echo "PASS: Rosetta binfmt registration is opt-in and restores qemu safely"
