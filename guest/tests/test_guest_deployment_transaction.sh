#!/bin/bash
set -eEuo pipefail

GUEST_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCRIPT="$GUEST_ROOT/scripts/guest-deployment-transaction.sh"
WORK=$(mktemp -d)
cleanup() {
    local status=$?
    if [ "$status" -ne 0 ]; then
        # Expected failures redirect their output. Keep unexpected assertion
        # failures diagnosable before deleting this disposable fixture.
        local output
        for output in "$WORK"/*.err "$WORK"/*.out; do
            [ -f "$output" ] || continue
            printf '\nFixture output: %s\n' "${output##*/}" >&2
            tail -n 40 "$output" >&2 || true
        done
    fi
    rm -rf "$WORK"
    exit "$status"
}
trap cleanup EXIT
trap 'printf "FAIL: guest deployment transaction test line %s: %s\n" "$LINENO" "$BASH_COMMAND" >&2' ERR

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

file_mode() {
    case "$(uname -s)" in
    Darwin) stat -f '%Lp' "$1" ;;
    *) stat -c '%a' "$1" ;;
    esac
}

write_file() {
    mkdir -p "${1%/*}"
    printf '%s\n' "$2" >"$1"
}

assert_file() {
    [ -f "$1" ] && [ ! -L "$1" ] || fail "missing regular file: $1"
    [ "$(cat "$1")" = "$2" ] || fail "unexpected content: $1"
}

# Runs ACTION on TOKEN with fresh sync logs that watch its transaction.
run_watched() {
    : >"$SYNC_LOG"
    : >"$SYNC_WATCH_LOG"
    SYNC_WATCH="$TRANSACTION_ROOT/$2" bash "$SCRIPT" "$1" "$2"
}

# Commit and rollback retire a backup in four barriers: the protected items,
# then the whole root filesystem (systemd enablement links and binfmt
# databases are outside the backup), both while the backup is still ready;
# then the removed phase alone; then everything else.
assert_retired_in_order() {
    local transaction="$TRANSACTION_ROOT/$1" path
    [ "$(wc -l <"$SYNC_LOG")" -eq 4 ] || fail "unexpected retirement barriers"
    for path in "$ROOT/usr/local/bin" "$ROOT/usr/local/bin/hamnd" \
        "$ROOT/etc/docker/daemon.json" "$ROOT/opt/cni/bin"; do
        head -n 1 "$SYNC_LOG" | tr ' ' '\n' | grep -Fqx -- "$path" ||
            fail "runtime entry was not synced before retirement: $path"
    done
    [ "$(sed -n 1p "$SYNC_WATCH_LOG")" = "phase=1 data=1" ] ||
        fail "runtime was not durable before the backup was retired"
    [ "$(sed -n 2p "$SYNC_LOG")" = "-f -- $ROOT/" ] &&
        [ "$(sed -n 2p "$SYNC_WATCH_LOG")" = "phase=1 data=1" ] ||
        fail "root filesystem was not durable before the backup was retired"
    [ "$(sed -n 3p "$SYNC_LOG")" = "-- $transaction" ] &&
        [ "$(sed -n 3p "$SYNC_WATCH_LOG")" = "phase=0 data=1" ] ||
        fail "phase was not retired durably before the backup data"
    [ "$(sed -n 4p "$SYNC_LOG")" = "-- $TRANSACTION_ROOT" ] &&
        [ "$(sed -n 4p "$SYNC_WATCH_LOG")" = "phase=0 data=0" ] ||
        fail "backup removal was not made durable"
}

mkdir -p "$WORK/root" "$WORK/bin"
ROOT=$(cd "$WORK/root" && pwd -P)
BIN="$WORK/bin"
export HAMN_DEPLOYMENT_TEST_ROOT="$ROOT"
export SYSTEMCTL_LOG="$WORK/systemctl.log"
export SYSCTL_LOG="$WORK/sysctl.log"
: >"$SYSTEMCTL_LOG"
: >"$SYSCTL_LOG"

cat >"$BIN/systemctl" <<'EOF'
#!/bin/bash
set -u
printf '%s\n' "$*" >>"$SYSTEMCTL_LOG"
case "${1:-}" in
is-enabled)
    case "${2:-}" in
    hamnd.service) printf 'enabled\n' ;;
    containerd.service) printf 'masked\n' ;;
    docker.service) printf 'disabled\n' ;;
    *) printf 'not-found\n' ;;
    esac
    ;;
is-active)
    [ "${2:-}" = --quiet ] || exit 92
    case "${3:-}" in
    hamnd.service|containerd.service) exit 0 ;;
    *) exit 3 ;;
    esac
    ;;
restart)
    if [ "${FAIL_SYSTEMCTL_RESTART:-0}" = 1 ] &&
       [ "${2:-}" = hamnd.service ]; then
        exit 91
    fi
    ;;
esac
exit 0
EOF
cat >"$BIN/sysctl" <<'EOF'
#!/bin/bash
set -u
printf '%s\n' "$*" >>"$SYSCTL_LOG"
[ "${FAIL_SYSCTL:-0}" != 1 ]
EOF
# One line of operands per sync(1) call; FAIL_SYNC_AT=N fails the Nth call,
# and FAIL_SYNC_AT="N M" each of those calls.
# With SYNC_WATCH set to a transaction, each call also records whether its
# phase and backup data still existed when that barrier ran.
export SYNC_LOG="$WORK/sync.log"
export SYNC_WATCH_LOG="$WORK/sync-watch.log"
: >"$SYNC_LOG"
cat >"$BIN/sync" <<'EOF'
#!/bin/bash
set -u
printf '%s\n' "$*" >>"$SYNC_LOG"
if [ -n "${SYNC_WATCH:-}" ]; then
    phase=0 data=0
    [ ! -e "$SYNC_WATCH/phase" ] || phase=1
    [ ! -e "$SYNC_WATCH/data" ] || data=1
    printf 'phase=%s data=%s\n' "$phase" "$data" >>"$SYNC_WATCH_LOG"
fi
case " ${FAIL_SYNC_AT:-} " in
*" $(wc -l <"$SYNC_LOG" | tr -d ' ') "*)
    echo 'injected durability failure' >&2
    exit 1
    ;;
esac
EOF
chmod +x "$BIN/systemctl" "$BIN/sysctl" "$BIN/sync"
export PATH="$BIN:$PATH"

write_file "$ROOT/usr/local/bin/hamnd" old-hamnd
chmod 0755 "$ROOT/usr/local/bin/hamnd"
write_file "$ROOT/usr/local/libexec/hamn/old-helper" old-helper
chmod 0710 "$ROOT/usr/local/libexec/hamn"
write_file "$ROOT/etc/systemd/system/hamnd.service" old-hamnd-unit
write_file "$ROOT/etc/hamn/runtime-state" old-state
write_file "$ROOT/etc/containerd/config.toml" old-containerd
write_file "$ROOT/etc/docker/daemon.json" old-docker
write_file "$ROOT/etc/systemd/system/docker.service.d/10-hamn-containerd.conf" old-dropin
write_file "$ROOT/etc/dnsmasq.d/hamn-host-dns.conf" old-host-dns-config
write_file "$ROOT/etc/systemd/system/hamn-host-dns.service" old-host-dns-unit
write_file "$ROOT/etc/modules-load.d/hamn-kubernetes.conf" old-modules
write_file "$ROOT/etc/sysctl.d/99-hamn-kubernetes.conf" old-sysctl
write_file "$ROOT/opt/cni/bin/old-plugin" old-cni

TOKEN=0123456789abcdef0123456789abcdef
TRANSACTION_ROOT="$ROOT/var/lib/hamn/deployment-transactions"
HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$TOKEN"
TRANSACTION="$TRANSACTION_ROOT/$TOKEN"
[ -d "$TRANSACTION" ] || fail "transaction was not created"
[ "$(file_mode "$TRANSACTION")" = 700 ] || fail "transaction mode"
[ "$(cat "$TRANSACTION/phase")" = ready ] || fail "transaction phase"
[ "$(file_mode "$TRANSACTION/phase")" = 600 ] || fail "phase mode"
[ ! -e "$TRANSACTION/.phase" ] || fail "unpublished phase remains"
# The first barrier covered every backup file and directory before the phase.
[ "$(wc -l <"$SYNC_LOG")" -eq 3 ] || fail "unexpected sync barriers"
while IFS= read -r entry; do
    head -n 1 "$SYNC_LOG" | tr ' ' '\n' | grep -Fqx -- "$entry" ||
        fail "backup entry was not synced: $entry"
done < <(find "$TRANSACTION" \( -type f -o -type d \) ! -name phase)
[ "$(sed -n 2p "$SYNC_LOG")" = "-- $TRANSACTION/.phase" ] || fail "phase was not synced"
[ "$(sed -n 3p "$SYNC_LOG")" = "-- $TRANSACTION $TRANSACTION_ROOT" ] ||
    fail "published phase directories were not synced"
for key in hamnd libexec_hamn hamnd_unit etc_hamn containerd_config \
    docker_config docker_dropin host_dns_config host_dns_unit modules_config \
    sysctl_config cni_bin; do
    [ -f "$TRANSACTION/meta/$key" ] || fail "missing metadata: $key"
done
test ! -e "$TRANSACTION/meta/hamn_engine"
test ! -e "$TRANSACTION/meta/nerdctl"

# A pending transaction blocks an unsafe second deployment.
if HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin \
    1123456789abcdef0123456789abcdef >"$WORK/pending.out" \
    2>"$WORK/pending.err"; then
    fail "second transaction was accepted"
fi
grep -Fq "pending deployment transaction $TOKEN" "$WORK/pending.err"

# Change every protected target, then verify exact restoration.
write_file "$ROOT/usr/local/bin/hamnd" new-hamnd
rm -rf "$ROOT/usr/local/libexec/hamn"
write_file "$ROOT/usr/local/libexec/hamn/new-helper" new-helper
write_file "$ROOT/etc/systemd/system/hamnd.service" new-hamnd-unit
rm -rf "$ROOT/etc/hamn"
write_file "$ROOT/etc/hamn/new-state" new-state
write_file "$ROOT/etc/containerd/config.toml" new-containerd
write_file "$ROOT/etc/docker/daemon.json" new-docker
rm -rf "$ROOT/etc/systemd/system/docker.service.d"
write_file "$ROOT/etc/systemd/system/docker.service.d/new.conf" new-dropin
write_file "$ROOT/etc/dnsmasq.d/hamn-host-dns.conf" new-host-dns-config
write_file "$ROOT/etc/systemd/system/hamn-host-dns.service" new-host-dns-unit
write_file "$ROOT/etc/modules-load.d/hamn-kubernetes.conf" new-modules
write_file "$ROOT/etc/sysctl.d/99-hamn-kubernetes.conf" new-sysctl
rm -rf "$ROOT/opt/cni/bin"
write_file "$ROOT/opt/cni/bin/new-plugin" new-cni

: >"$SYSTEMCTL_LOG"
: >"$SYSCTL_LOG"
run_watched rollback "$TOKEN"
assert_retired_in_order "$TOKEN"
head -n 1 "$SYNC_LOG" | tr ' ' '\n' |
    grep -Fqx -- "$ROOT/opt/cni/bin/old-plugin" ||
    fail "restored runtime was not synced before retirement"
assert_file "$ROOT/usr/local/bin/hamnd" old-hamnd
assert_file "$ROOT/usr/local/libexec/hamn/old-helper" old-helper
test ! -e "$ROOT/usr/local/libexec/hamn/new-helper"
assert_file "$ROOT/etc/systemd/system/hamnd.service" old-hamnd-unit
assert_file "$ROOT/etc/hamn/runtime-state" old-state
assert_file "$ROOT/etc/containerd/config.toml" old-containerd
assert_file "$ROOT/etc/docker/daemon.json" old-docker
assert_file "$ROOT/etc/systemd/system/docker.service.d/10-hamn-containerd.conf" old-dropin
test ! -e "$ROOT/etc/systemd/system/docker.service.d/new.conf"
assert_file "$ROOT/etc/dnsmasq.d/hamn-host-dns.conf" old-host-dns-config
assert_file "$ROOT/etc/systemd/system/hamn-host-dns.service" old-host-dns-unit
assert_file "$ROOT/etc/modules-load.d/hamn-kubernetes.conf" old-modules
assert_file "$ROOT/etc/sysctl.d/99-hamn-kubernetes.conf" old-sysctl
assert_file "$ROOT/opt/cni/bin/old-plugin" old-cni
test ! -e "$ROOT/opt/cni/bin/new-plugin"
grep -Fxq 'daemon-reload' "$SYSTEMCTL_LOG"
grep -Fxq 'unmask containerd.service' "$SYSTEMCTL_LOG"
grep -Fxq 'restart containerd.service' "$SYSTEMCTL_LOG"
grep -Fxq 'mask containerd.service' "$SYSTEMCTL_LOG"
grep -Fxq 'unmask docker.service' "$SYSTEMCTL_LOG"
grep -Fxq 'disable docker.service' "$SYSTEMCTL_LOG"
grep -Fxq 'stop docker.service' "$SYSTEMCTL_LOG"
grep -Fxq -- '--system' "$SYSCTL_LOG"
test ! -e "$TRANSACTION"

# Commit removes only the backup, never the installed data, and makes the
# deployed runtime durable before it retires the backup.
COMMIT=2123456789abcdef0123456789abcdef
HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$COMMIT"
write_file "$ROOT/usr/local/bin/hamnd" committed-hamnd
run_watched commit "$COMMIT"
assert_retired_in_order "$COMMIT"
assert_file "$ROOT/usr/local/bin/hamnd" committed-hamnd
test ! -e "$TRANSACTION_ROOT/$COMMIT"

# An interrupted commit leaves either a ready, intact backup (its runtime was
# not yet durable) or a retired one that is never restored.
INTERRUPTED=5123456789abcdef0123456789abcdef
for failure in 1 2; do
    HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$INTERRUPTED"
    write_file "$ROOT/usr/local/bin/hamnd" interrupted-hamnd
    if FAIL_SYNC_AT="$failure" run_watched commit "$INTERRUPTED" \
        >"$WORK/commit.out" 2>"$WORK/commit.err"; then
        fail "commit accepted a runtime that was not durable at barrier $failure"
    fi
    grep -Fq 'cannot make the deployed runtime durable; backup retained' "$WORK/commit.err"
    [ "$(cat "$TRANSACTION_ROOT/$INTERRUPTED/phase")" = ready ] ||
        fail "commit retired a backup before its runtime was durable"
    bash "$SCRIPT" rollback "$INTERRUPTED"
    assert_file "$ROOT/usr/local/bin/hamnd" committed-hamnd
done

HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$INTERRUPTED"
write_file "$ROOT/usr/local/bin/hamnd" interrupted-hamnd
if FAIL_SYNC_AT=3 run_watched commit "$INTERRUPTED" \
    >"$WORK/commit.out" 2>"$WORK/commit.err"; then
    fail "commit accepted a retirement that was not durable"
fi
grep -Fq "retired the backup at $TRANSACTION_ROOT/$INTERRUPTED but cannot make that durable" \
    "$WORK/commit.err"
test ! -e "$TRANSACTION_ROOT/$INTERRUPTED/phase"
test -d "$TRANSACTION_ROOT/$INTERRUPTED/data"
if bash "$SCRIPT" rollback "$INTERRUPTED" >"$WORK/retired.out" \
    2>"$WORK/retired.err"; then
    fail "a retired backup was restored"
fi
grep -Fq 'transaction is incomplete' "$WORK/retired.err"
assert_file "$ROOT/usr/local/bin/hamnd" interrupted-hamnd
rm -rf "${TRANSACTION_ROOT:?}/$INTERRUPTED"

HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$INTERRUPTED"
if FAIL_SYNC_AT=4 run_watched commit "$INTERRUPTED" \
    >"$WORK/commit.out" 2>"$WORK/commit.err"; then
    fail "commit accepted a removal that was not durable"
fi
grep -Fq 'removed the retired backup but cannot make the removal durable' \
    "$WORK/commit.err"
test ! -e "$TRANSACTION_ROOT/$INTERRUPTED"
assert_file "$ROOT/usr/local/bin/hamnd" interrupted-hamnd
write_file "$ROOT/usr/local/bin/hamnd" committed-hamnd

# A crash can leave a transaction whose phase is missing, empty or
# zero-filled. It blocks the next begin with its own state, and neither
# rollback nor commit acts on it.
UNPUBLISHED=6123456789abcdef0123456789abcdef
for damage in zero-filled empty missing trailing-nul; do
    HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$UNPUBLISHED"
    phase="$TRANSACTION_ROOT/$UNPUBLISHED/phase"
    case "$damage" in
    zero-filled) head -c 6 /dev/zero >"$phase" ;;
    empty) : >"$phase" ;;
    missing) rm "$phase" ;;
    trailing-nul) printf 'ready\0' >"$phase" ;;
    esac
    write_file "$ROOT/usr/local/bin/hamnd" unpublished-hamnd
    if bash "$SCRIPT" begin 7123456789abcdef0123456789abcdef \
        >"$WORK/unpublished.out" 2>"$WORK/unpublished-begin.err"; then
        fail "begin accepted a $damage leftover"
    fi
    grep -Fq "deployment transaction $UNPUBLISHED at $TRANSACTION_ROOT/$UNPUBLISHED has no ready backup; it is never restored" \
        "$WORK/unpublished-begin.err" || fail "$damage leftover was not named"
    for action in rollback commit; do
        if bash "$SCRIPT" "$action" "$UNPUBLISHED" >"$WORK/unpublished.out" \
            2>"$WORK/unpublished-$action.err"; then
            fail "$action accepted a $damage phase"
        fi
        grep -Fq 'transaction is incomplete' "$WORK/unpublished-$action.err"
    done
    ! grep -Fq 'null byte' "$WORK"/unpublished-*.err ||
        fail "$damage phase was read through command substitution"
    test -d "$TRANSACTION_ROOT/$UNPUBLISHED/data"
    test ! -e "$TRANSACTION_ROOT/7123456789abcdef0123456789abcdef"
    assert_file "$ROOT/usr/local/bin/hamnd" unpublished-hamnd
    rm -rf "${TRANSACTION_ROOT:?}/$UNPUBLISHED"
done
HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$UNPUBLISHED"
rm "$TRANSACTION_ROOT/$UNPUBLISHED/phase"
mkdir "$TRANSACTION_ROOT/$UNPUBLISHED/phase"
if bash "$SCRIPT" begin 7123456789abcdef0123456789abcdef \
    >"$WORK/unpublished.out" 2>"$WORK/unpublished.err"; then
    fail "begin accepted an unsafe phase"
fi
grep -Fq "deployment transaction $UNPUBLISHED has an unsafe phase; inspect" \
    "$WORK/unpublished.err"
rm -rf "${TRANSACTION_ROOT:?}/$UNPUBLISHED"
write_file "$ROOT/usr/local/bin/hamnd" committed-hamnd

# A phase that cannot be read may belong to a ready backup. begin, rollback
# and commit name it and neither restore nor remove anything; once it reads
# again, the intact backup still rolls back.
HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$UNPUBLISHED"
write_file "$ROOT/usr/local/bin/hamnd" unreadable-hamnd
chmod 000 "$TRANSACTION_ROOT/$UNPUBLISHED/phase"
if bash "$SCRIPT" begin 7123456789abcdef0123456789abcdef \
    >"$WORK/unreadable.out" 2>"$WORK/unreadable-begin.err"; then
    fail "begin accepted an unreadable phase"
fi
grep -Fq "cannot read the phase of deployment transaction $UNPUBLISHED; inspect" \
    "$WORK/unreadable-begin.err"
for action in rollback commit; do
    if bash "$SCRIPT" "$action" "$UNPUBLISHED" >"$WORK/unreadable.out" \
        2>"$WORK/unreadable-$action.err"; then
        fail "$action accepted an unreadable phase"
    fi
    grep -Fq 'cannot read the transaction phase' "$WORK/unreadable-$action.err"
done
assert_file "$ROOT/usr/local/bin/hamnd" unreadable-hamnd
test -d "$TRANSACTION_ROOT/$UNPUBLISHED/data"
chmod 0600 "$TRANSACTION_ROOT/$UNPUBLISHED/phase"
bash "$SCRIPT" rollback "$UNPUBLISHED"
assert_file "$ROOT/usr/local/bin/hamnd" committed-hamnd
test ! -e "$TRANSACTION_ROOT/$UNPUBLISHED"

# Protected source symlinks are rejected before backup mutation.
mv "$ROOT/etc/docker" "$ROOT/etc/docker-real"
ln -s docker-real "$ROOT/etc/docker"
if HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin \
    3123456789abcdef0123456789abcdef >"$WORK/symlink.out" \
    2>"$WORK/symlink.err"; then
    fail "transaction followed a protected Docker symlink"
fi
grep -Fq 'refusing symlink parent: /etc/docker' "$WORK/symlink.err"
rm "$ROOT/etc/docker"
mv "$ROOT/etc/docker-real" "$ROOT/etc/docker"

# A failed durability barrier must never publish a recoverable ready backup.
# Each sync(1) call is one barrier: backup contents, the unpublished phase,
# then the directories that hold the published phase. A failure at the third
# barrier comes after ready was published: the failed begin then retires its
# phase as commit and rollback do, removed and made durable (a fourth sync)
# while the backup data still exists, and only then removes the rest.
FAILED_BEGIN=4123456789abcdef0123456789abcdef
for failure in 1 2 3; do
    : >"$SYNC_LOG"
    : >"$SYNC_WATCH_LOG"
    if SYNC_WATCH="$TRANSACTION_ROOT/$FAILED_BEGIN" FAIL_SYNC_AT="$failure" \
        bash "$SCRIPT" begin "$FAILED_BEGIN" >"$WORK/sync.out" 2>"$WORK/sync.err"; then
        fail "accepted a failed durability barrier"
    fi
    grep -Fq 'injected durability failure' "$WORK/sync.err"
    expected=$failure
    [ "$failure" -ne 3 ] || expected=4
    [ "$(wc -l <"$SYNC_LOG")" -eq "$expected" ] ||
        fail "barrier $failure: unexpected sync barriers"
    if [ "$failure" -eq 3 ]; then
        [ "$(sed -n 4p "$SYNC_LOG")" = "-- $TRANSACTION_ROOT/$FAILED_BEGIN" ] ||
            fail "the failed begin did not make its phase retirement durable"
        [ "$(sed -n 4p "$SYNC_WATCH_LOG")" = "phase=0 data=1" ] ||
            fail "the failed begin removed backup data before retiring its phase"
    fi
    test ! -e "$TRANSACTION_ROOT/$FAILED_BEGIN"
    assert_file "$ROOT/usr/local/bin/hamnd" committed-hamnd
done
# When that retirement cannot be made durable either, the backup stays whole
# without its phase: recovery discards it, and a crash that brought the phase
# back would leave a complete backup, durable since before its publication.
: >"$SYNC_LOG"
if FAIL_SYNC_AT="3 4" bash "$SCRIPT" begin "$FAILED_BEGIN" \
    >"$WORK/sync.out" 2>"$WORK/sync.err"; then
    fail "accepted a failed durability barrier"
fi
[ "$(wc -l <"$SYNC_LOG")" -eq 4 ] || fail "unexpected sync barriers"
[ -d "$TRANSACTION_ROOT/$FAILED_BEGIN/data" ] &&
    [ -d "$TRANSACTION_ROOT/$FAILED_BEGIN/meta" ] ||
    fail "the failed begin removed a backup whose phase retirement was not durable"
[ ! -e "$TRANSACTION_ROOT/$FAILED_BEGIN/phase" ] ||
    fail "the failed begin kept a ready phase"
rm -rf -- "${TRANSACTION_ROOT:?}/$FAILED_BEGIN"
assert_file "$ROOT/usr/local/bin/hamnd" committed-hamnd

echo "PASS: Docker/CRI guest deployment transactions roll back atomically"
