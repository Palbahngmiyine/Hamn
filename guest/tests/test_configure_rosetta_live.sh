#!/bin/bash
# Opt-in only, in an exclusively owned disposable Linux guest with Rosetta
# already shared by Virtualization.framework. Never installs a host runtime.
set -euo pipefail

[ "${HAMN_ROSETTA_LIVE_TEST:-}" = 1 ] || {
    echo 'Set HAMN_ROSETTA_LIVE_TEST=1 only in a disposable owned guest' >&2
    exit 2
}
[ "$(uname -s)" = Linux ] && [ "$EUID" -eq 0 ] || exit 2
SCRIPT=${HAMN_ROSETTA_SCRIPT:-/usr/local/libexec/hamn/configure-rosetta}
IMAGE=${HAMN_ROSETTA_TEST_AMD64_IMAGE:?immutable pre-pulled amd64 image ID required}
[[ "$IMAGE" =~ ^sha256:[0-9a-f]{64}$ ]] || exit 2
test -x /mnt/hamn-rosetta/rosetta
test -x "$SCRIPT"
REAL_UPDATE=$(command -v update-binfmts)
WORK=$(mktemp -d /var/tmp/hamn-rosetta-live.XXXXXX)
HANDLER=hamn-rosetta-proof-${WORK##*.}
DATABASE=$WORK/binfmts
test ! -e "/proc/sys/fs/binfmt_misc/$HANDLER"
test ! -e "/var/lib/binfmts/$HANDLER"

configure() {
    HAMN_ROSETTA_HANDLER="$HANDLER" HAMN_ROSETTA_BINFMT_DIR="$DATABASE" \
        bash "$SCRIPT" "$@"
}

cleanup() {
    local rc=$?
    trap - EXIT
    set +e
    # Clear only this test's unique entries, then restore the package qemu
    # handler. The script owns only its database; an entry for this handler
    # in the default database is a regression, removed here and reported.
    if [ -d "$DATABASE" ]; then
        "$REAL_UPDATE" --admindir "$DATABASE" --remove "$HANDLER" \
            /mnt/hamn-rosetta/rosetta || rc=1
    fi
    if [ -f "/var/lib/binfmts/$HANDLER" ]; then
        echo "FAIL: Rosetta configuration wrote the default binfmt database" >&2
        "$REAL_UPDATE" --remove "$HANDLER" /mnt/hamn-rosetta/rosetta
        rc=1
    fi
    "$REAL_UPDATE" --enable qemu-x86_64 || rc=1
    test ! -e "/proc/sys/fs/binfmt_misc/$HANDLER" || rc=1
    grep -Fxq enabled /proc/sys/fs/binfmt_misc/qemu-x86_64 || rc=1
    rm -rf "$WORK"
    exit "$rc"
}
trap cleanup EXIT

amd64_runs() {
    test "$(docker run --rm --pull=never --platform=linux/amd64 "$IMAGE" uname -m)" = x86_64
}

qemu_restored() {
    test ! -e "/proc/sys/fs/binfmt_misc/$HANDLER"
    test ! -e "$DATABASE/$HANDLER"
    test ! -e "/var/lib/binfmts/$HANDLER"
    grep -Fxq enabled /proc/sys/fs/binfmt_misc/qemu-x86_64
    amd64_runs
}

# The normal handler belongs to this exclusively owned guest. Switch it off
# before testing a unique handler so the independent observer is unambiguous.
bash "$SCRIPT" disable
amd64_runs
configure enable
grep -Fxq enabled "/proc/sys/fs/binfmt_misc/$HANDLER"
grep -Fxq 'interpreter /mnt/hamn-rosetta/rosetta' "/proc/sys/fs/binfmt_misc/$HANDLER"
test ! -e /proc/sys/fs/binfmt_misc/qemu-x86_64
amd64_runs
configure enable
test "$(find "$DATABASE" -type f | wc -l)" -eq 1
amd64_runs
configure disable
qemu_restored
echo 'PASS: real Rosetta registration, repeated enable, amd64 execution and disable'

# The wrapper only injects one boundary failure; all actual registrations,
# removals and qemu restorations still use the distro update-binfmts binary.
cat >"$WORK/update-binfmts" <<'EOF'
#!/bin/bash
set -euo pipefail
args=("$@")
if [ "${1:-}" = --admindir ]; then shift 2; fi
case "${HAMN_ROSETTA_FAIL_STAGE:?}:${1:-}" in
    before-install:--install) exit 91 ;;
    after-install:--install|after-disable:--disable)
        "${HAMN_ROSETTA_REAL_UPDATE:?}" "${args[@]}"
        exit 92
        ;;
esac
exec "${HAMN_ROSETTA_REAL_UPDATE:?}" "${args[@]}"
EOF
chmod 0755 "$WORK/update-binfmts"
for failure in before-install after-install after-disable; do
    configure enable
    if HAMN_UPDATE_BINFMT="$WORK/update-binfmts" \
        HAMN_ROSETTA_REAL_UPDATE="$REAL_UPDATE" HAMN_ROSETTA_FAIL_STAGE=$failure \
        configure enable >"$WORK/$failure.log" 2>&1; then
        echo "FAIL: accepted injected failure: $failure" >&2
        exit 1
    fi
    qemu_restored
    cat "$WORK/$failure.log"
    echo "PASS: real registration rollback with $failure"
done
