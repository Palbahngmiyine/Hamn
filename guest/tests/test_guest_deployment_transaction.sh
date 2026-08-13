#!/bin/bash
set -euo pipefail

GUEST_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCRIPT="$GUEST_ROOT/scripts/guest-deployment-transaction.sh"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

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
chmod +x "$BIN/systemctl" "$BIN/sysctl"
export PATH="$BIN:$PATH"

write_file "$ROOT/usr/local/bin/hamnd" old-hamnd
chmod 0755 "$ROOT/usr/local/bin/hamnd"
write_file "$ROOT/usr/local/libexec/hamn/old-helper" old-helper
chmod 0710 "$ROOT/usr/local/libexec/hamn"
write_file "$ROOT/etc/systemd/system/hamnd.service" old-hamnd-unit
write_file "$ROOT/etc/systemd/system/k3s.service" old-k3s-unit
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
for key in hamnd libexec_hamn hamnd_unit k3s_unit etc_hamn containerd_config \
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
write_file "$ROOT/etc/systemd/system/k3s.service" new-k3s-unit
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
HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" rollback "$TOKEN"
assert_file "$ROOT/usr/local/bin/hamnd" old-hamnd
assert_file "$ROOT/usr/local/libexec/hamn/old-helper" old-helper
test ! -e "$ROOT/usr/local/libexec/hamn/new-helper"
assert_file "$ROOT/etc/systemd/system/hamnd.service" old-hamnd-unit
assert_file "$ROOT/etc/systemd/system/k3s.service" old-k3s-unit
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

# Commit removes only the backup, never the installed data.
COMMIT=2123456789abcdef0123456789abcdef
HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" begin "$COMMIT"
write_file "$ROOT/usr/local/bin/hamnd" committed-hamnd
HAMN_DEPLOYMENT_TEST_ROOT="$ROOT" bash "$SCRIPT" commit "$COMMIT"
assert_file "$ROOT/usr/local/bin/hamnd" committed-hamnd
test ! -e "$TRANSACTION_ROOT/$COMMIT"

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

echo "PASS: Docker/CRI guest deployment transactions roll back atomically"
