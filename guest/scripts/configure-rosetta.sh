#!/bin/bash
# Configure the guest's x86_64 binfmt handler. The shared Rosetta runtime is
# opt-in; qemu-x86_64 remains the image default and is restored on disable.
set -euo pipefail
export LC_ALL=C

UPDATE_BINFMT=${HAMN_UPDATE_BINFMT:-update-binfmts}
MOUNT=${HAMN_MOUNT:-mount}
INSTALL=${HAMN_INSTALL:-install}
MOUNT_TABLE=${HAMN_PROC_MOUNTS:-/proc/mounts}
ROSETTA_TAG=${HAMN_ROSETTA_TAG:-rosetta}
MOUNT_POINT=${HAMN_ROSETTA_MOUNT_POINT:-/mnt/hamn-rosetta}
ROSETTA_HANDLER=${HAMN_ROSETTA_HANDLER:-hamn-rosetta}
QEMU_HANDLER=${HAMN_QEMU_HANDLER:-qemu-x86_64}
PRESERVE=${HAMN_ROSETTA_PRESERVE:-no}
# update-binfmts rejects equal magic in one database with fix-binary, even
# when the other handler is disabled. Keep the package-owned qemu database
# intact; Rosetta owns only this separate database and its kernel handler.
ROSETTA_BINFMT_DIR=${HAMN_ROSETTA_BINFMT_DIR:-/var/lib/hamn/binfmts}

MAGIC='\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00'
MASK='\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff'

fail() {
    echo "hamn: Rosetta configuration: $*" >&2
    exit 1
}

usage() {
    echo "usage: configure-rosetta enable|disable" >&2
    exit 2
}

[ "$#" -eq 1 ] || usage
case "$1" in
    enable|disable) ;;
    *) usage ;;
esac

case "$PRESERVE" in
    yes|no) ;;
    *) fail "HAMN_ROSETTA_PRESERVE must be yes or no" ;;
esac

command -v "$UPDATE_BINFMT" >/dev/null 2>&1 ||
    fail "update-binfmts is missing from the guest image"

handler_exists() {
    "$UPDATE_BINFMT" --display "$1" >/dev/null 2>&1
}

rosetta_binfmt() {
    "$UPDATE_BINFMT" --admindir "$ROSETTA_BINFMT_DIR" "$@"
}

remove_rosetta_handler() {
    if rosetta_binfmt --display "$ROSETTA_HANDLER" >/dev/null 2>&1; then
        rosetta_binfmt --remove "$ROSETTA_HANDLER" "$MOUNT_POINT/rosetta" ||
            fail "cannot remove Rosetta binfmt handler"
    fi
}

ensure_qemu_handler() {
    handler_exists "$QEMU_HANDLER" ||
        fail "qemu x86_64 binfmt handler is unavailable: $QEMU_HANDLER"
    "$UPDATE_BINFMT" --enable "$QEMU_HANDLER" ||
        fail "cannot enable qemu x86_64 binfmt handler"
}

rosetta_is_mounted() {
    [ -r "$MOUNT_TABLE" ] || return 1
    awk -v tag="$ROSETTA_TAG" -v target="$MOUNT_POINT" \
        '$1 == tag && $2 == target && $3 == "virtiofs" { found = 1 }
         END { exit found ? 0 : 1 }' "$MOUNT_TABLE"
}

ensure_rosetta_mount() {
    [ ! -L "$MOUNT_POINT" ] || fail "Rosetta mount point is a symlink"
    if [ ! -e "$MOUNT_POINT" ]; then
        "$INSTALL" -d -m 0755 "$MOUNT_POINT" ||
            fail "cannot create Rosetta mount point"
    fi
    [ -d "$MOUNT_POINT" ] || fail "Rosetta mount point is not a directory"
    if ! rosetta_is_mounted; then
        "$MOUNT" -t virtiofs "$ROSETTA_TAG" "$MOUNT_POINT" ||
            fail "cannot mount the Rosetta virtiofs share"
    fi
}

enable_rosetta() {
    ensure_rosetta_mount
    local runtime="$MOUNT_POINT/rosetta"
    [ -f "$runtime" ] && [ ! -L "$runtime" ] && [ -x "$runtime" ] ||
        fail "Rosetta runtime is unavailable at $runtime"
    handler_exists "$QEMU_HANDLER" ||
        fail "qemu x86_64 binfmt handler is unavailable: $QEMU_HANDLER"

    # Reconfiguration can start with qemu disabled. Establish the fallback
    # before replacing the old Rosetta registration or attempting a new one.
    ensure_qemu_handler
    remove_rosetta_handler
    rosetta_binfmt --install "$ROSETTA_HANDLER" "$runtime" \
        --magic "$MAGIC" --mask "$MASK" --credentials yes \
        --preserve "$PRESERVE" --fix-binary yes || {
        ensure_qemu_handler
        remove_rosetta_handler
        fail "cannot register the Rosetta runtime"
    }
    if ! "$UPDATE_BINFMT" --disable "$QEMU_HANDLER"; then
        ensure_qemu_handler
        remove_rosetta_handler
        fail "cannot disable qemu while Rosetta is active"
    fi
    if ! rosetta_binfmt --display "$ROSETTA_HANDLER" |
        grep -Fxq "$ROSETTA_HANDLER (enabled):"; then
        ensure_qemu_handler
        remove_rosetta_handler
        fail "Rosetta registration did not persist"
    fi
    echo "hamn: Rosetta x86_64 translation is enabled"
}

disable_rosetta() {
    # Make qemu available before removing Rosetta so an interruption never
    # leaves the guest without an x86_64 ELF handler.
    ensure_qemu_handler
    remove_rosetta_handler
    echo "hamn: qemu x86_64 translation is enabled"
}

[ ! -L "$ROSETTA_BINFMT_DIR" ] || fail "Rosetta binfmt database is a symlink"
"$INSTALL" -d -m 0755 "$ROSETTA_BINFMT_DIR" ||
    fail "cannot create Rosetta binfmt database"
[ -d "$ROSETTA_BINFMT_DIR" ] || fail "Rosetta binfmt database is not a directory"

case "$1" in
    enable) enable_rosetta ;;
    disable) disable_rosetta ;;
esac
