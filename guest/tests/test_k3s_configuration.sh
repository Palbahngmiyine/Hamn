#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
UNIT="$ROOT/systemd/k3s.service"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

describe_path() {
    local path=$1

    if [ -L "$path" ]; then
        printf 'symlink -> %s' "$(readlink "$path")"
    elif [ -d "$path" ]; then
        printf 'directory'
    elif [ -f "$path" ]; then
        printf 'regular file'
    elif [ -e "$path" ]; then
        printf 'other filesystem entry'
    else
        printf 'missing'
    fi
}

assert_exit_status() {
    local name=$1
    local expected=$2
    local actual=$3

    [ "$actual" -eq "$expected" ] ||
        fail "$name: expected=$expected actual=$actual"
}

assert_directory() {
    local name=$1
    local path=$2

    [ -d "$path" ] ||
        fail "$name: expected=directory actual=$(describe_path "$path") path=$path"
}

assert_symlink() {
    local name=$1
    local path=$2

    [ -L "$path" ] ||
        fail "$name: expected=symlink actual=$(describe_path "$path") path=$path"
}

bash -n "$ROOT/scripts/install-k3s.sh"
bash -n "$ROOT/scripts/configure-k3s.sh"
grep -Fq -- '--container-runtime-endpoint=unix:///run/containerd/containerd.sock' "$UNIT"
grep -Fq -- '--image-service-endpoint=unix:///run/containerd/containerd.sock' "$UNIT"
grep -Fq -- '--cluster-cidr=10.42.0.0/16' "$UNIT"
grep -Fq -- '--service-cidr=10.43.0.0/16' "$UNIT"
if grep -q '/run/k3s/containerd' "$UNIT"; then
    echo "FAIL: k3s service references an embedded containerd socket" >&2
    exit 1
fi
grep -Fq 'hamn-k3s-compatibility' "$ROOT/scripts/install-k3s.sh"
grep -Fq 'air-gap images' "$ROOT/scripts/install-k3s.sh"
grep -Fq '/etc/cni/net.d/10-flannel.conflist' "$ROOT/scripts/configure-k3s.sh"
grep -Fq '/opt/cni/bin/flannel' "$ROOT/scripts/configure-k3s.sh"
grep -Fq 'systemctl restart containerd.service' "$ROOT/scripts/configure-k3s.sh"
if grep -q 'touch.*k3s-external-cni' "$ROOT/scripts/configure-k3s.sh"; then
    echo "FAIL: k3s CNI recovery still relies on a marker" >&2
    exit 1
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
SNAPSHOT_ROOT="$WORK/snapshots"
CNI_TRANSACTION="$SNAPSHOT_ROOT/transaction"
mkdir "$SNAPSHOT_ROOT"
mkdir -p "$WORK/source" "$WORK/etc/cni/net.d" "$WORK/opt/cni/bin"
printf '{}\n' >"$WORK/source/10-flannel.conflist"
printf '#!/bin/sh\nexit 0\n' >"$WORK/source/flannel"
printf '#!/bin/sh\nexit 0\n' >"$WORK/source/bandwidth"
chmod +x "$WORK/source/flannel" "$WORK/source/bandwidth"

repair_cni() {
    HAMN_K3S_CNI_CONFIG_SOURCE="$WORK/source/10-flannel.conflist" \
    HAMN_K3S_FLANNEL_SOURCE="$WORK/source/flannel" \
    HAMN_K3S_BANDWIDTH_SOURCE="$WORK/source/bandwidth" \
    HAMN_CNI_CONFIG_LINK="$WORK/etc/cni/net.d/10-flannel.conflist" \
    HAMN_FLANNEL_LINK="$WORK/opt/cni/bin/flannel" \
    HAMN_BANDWIDTH_LINK="$WORK/opt/cni/bin/bandwidth" \
    HAMN_K3S_CNI_TRANSACTION_DIR="$CNI_TRANSACTION" \
        bash "$ROOT/scripts/configure-k3s.sh" repair-cni
}

repair_cni
[ "$(readlink "$WORK/etc/cni/net.d/10-flannel.conflist")" = \
    "$WORK/source/10-flannel.conflist" ]
[ "$(readlink "$WORK/opt/cni/bin/flannel")" = "$WORK/source/flannel" ]
[ "$(readlink "$WORK/opt/cni/bin/bandwidth")" = "$WORK/source/bandwidth" ]

flannel_inode=$(ls -di "$WORK/opt/cni/bin/flannel" | awk '{ print $1 }')
repair_cni
[ "$(ls -di "$WORK/opt/cni/bin/flannel" | awk '{ print $1 }')" = \
    "$flannel_inode" ]

ln -sfn "$WORK/source/bandwidth" "$WORK/opt/cni/bin/flannel"
rm "$WORK/opt/cni/bin/bandwidth"
repair_cni
[ "$(readlink "$WORK/opt/cni/bin/flannel")" = "$WORK/source/flannel" ]

# SIGKILL after an atomic link publish must leave a durable snapshot. The next
# invocation restores the exact symlink/regular-file state before validating
# new sources.
OLD_CONFIG="$WORK/source/old-crash.conflist"
OLD_BANDWIDTH="$WORK/source/old-bandwidth"
printf '{"old":"crash"}\n' >"$OLD_CONFIG"
printf '#!/bin/sh\nexit 0\n' >"$OLD_BANDWIDTH"
chmod +x "$OLD_BANDWIDTH"
ln -sfn "$OLD_CONFIG" "$WORK/etc/cni/net.d/10-flannel.conflist"
rm -f "$WORK/opt/cni/bin/flannel" "$WORK/opt/cni/bin/bandwidth"
printf 'old crash flannel\n' >"$WORK/opt/cni/bin/flannel"
chmod 0640 "$WORK/opt/cni/bin/flannel"
ln -s "$OLD_BANDWIDTH" "$WORK/opt/cni/bin/bandwidth"
set +e
HAMN_TEST=1 HAMN_TEST_CNI_LINK_KILL_AFTER=2 \
    HAMN_K3S_CNI_CONFIG_SOURCE="$WORK/source/10-flannel.conflist" \
    HAMN_K3S_FLANNEL_SOURCE="$WORK/source/flannel" \
    HAMN_K3S_BANDWIDTH_SOURCE="$WORK/source/bandwidth" \
    HAMN_CNI_CONFIG_LINK="$WORK/etc/cni/net.d/10-flannel.conflist" \
    HAMN_FLANNEL_LINK="$WORK/opt/cni/bin/flannel" \
    HAMN_BANDWIDTH_LINK="$WORK/opt/cni/bin/bandwidth" \
    HAMN_K3S_CNI_TRANSACTION_DIR="$CNI_TRANSACTION" \
    bash "$ROOT/scripts/configure-k3s.sh" repair-cni \
    >"$WORK/cni-kill.out" 2>"$WORK/cni-kill.error"
kill_rc=$?
set -e
assert_exit_status "fault-injected CNI repair exit status" 137 "$kill_rc"
assert_directory "durable CNI transaction snapshot" "$CNI_TRANSACTION"
assert_symlink "published CNI configuration" \
    "$WORK/etc/cni/net.d/10-flannel.conflist"
assert_symlink "published flannel plugin" "$WORK/opt/cni/bin/flannel"
assert_symlink "published bandwidth plugin" "$WORK/opt/cni/bin/bandwidth"
mv "$WORK/source/flannel" "$WORK/source/flannel.saved"
mkdir "$WORK/source/flannel"
if repair_cni 2>"$WORK/cni-recovery.error"; then
    echo "FAIL: invalid source was accepted after CNI recovery" >&2
    exit 1
fi
test ! -e "$CNI_TRANSACTION"
test "$(readlink "$WORK/etc/cni/net.d/10-flannel.conflist")" = \
    "$OLD_CONFIG"
test ! -L "$WORK/opt/cni/bin/flannel"
test "$(cat "$WORK/opt/cni/bin/flannel")" = "old crash flannel"
test -n "$(find "$WORK/opt/cni/bin/flannel" -prune -perm 0640 -print)"
test "$(readlink "$WORK/opt/cni/bin/bandwidth")" = "$OLD_BANDWIDTH"
rmdir "$WORK/source/flannel"
mv "$WORK/source/flannel.saved" "$WORK/source/flannel"
repair_cni
[ "$(readlink "$WORK/opt/cni/bin/bandwidth")" = "$WORK/source/bandwidth" ]

rm "$WORK/opt/cni/bin/flannel" "$WORK/source/flannel"
mkdir "$WORK/source/flannel"
if repair_cni 2>"$WORK/source-directory.error"; then
    echo "FAIL: k3s CNI source directory was accepted as a plugin" >&2
    exit 1
fi
grep -q 'k3s external CNI assets are incomplete' \
    "$WORK/source-directory.error"
rmdir "$WORK/source/flannel"
printf '#!/bin/sh\nexit 0\n' >"$WORK/source/flannel"
chmod +x "$WORK/source/flannel"
repair_cni
[ "$(readlink "$WORK/opt/cni/bin/flannel")" = "$WORK/source/flannel" ]

# CRI readiness does not prove that every base CNI executable still exists.
# A repeated k3s start must always run the idempotent CNI ensure path first.
START_SOURCE="$WORK/start-source"
START_DEST="$WORK/start-dest"
START_BIN="$WORK/start-bin"
mkdir -p "$START_SOURCE" "$START_DEST" "$START_BIN"
for plugin in bridge host-local loopback portmap firewall tuning; do
    printf '#!/bin/sh\nexit 0\n' >"$START_SOURCE/$plugin"
    chmod +x "$START_SOURCE/$plugin"
done
HAMN_CNI_SOURCE_DIR="$START_SOURCE" HAMN_CNI_BIN_DIR="$START_DEST" \
    bash "$ROOT/scripts/configure-containerd.sh" --ensure-cni
rm "$START_DEST/bridge"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "io.containerd.grpc.v1.cri linux/arm64 ok\n"' \
    >"$START_BIN/ctr"
cat >"$START_BIN/systemctl" <<'EOF'
#!/bin/bash
set -euo pipefail
read -r active enabled <"$SYSTEMCTL_STATE"
if [ -n "${SYSTEMCTL_QUERY_FAILURE:-}" ] &&
   { [ "$1" = is-active ] || [ "$1" = is-enabled ]; }; then
    exit 5
fi
case "$1" in
is-active) [ "$active" = 1 ] && exit 0 || exit 3 ;;
is-enabled) [ "$enabled" = 1 ]; exit ;;
enable) enabled=1; [ "${2:-}" != --now ] || active=1 ;;
disable) enabled=0; [ "${2:-}" != --now ] || active=0 ;;
start) active=1 ;;
stop) active=0 ;;
restart|status) ;;
*) exit 2 ;;
esac
printf '%s %s\n' "$active" "$enabled" >"$SYSTEMCTL_STATE"
EOF
printf '%s\n' '#!/bin/sh' 'exit 0' >"$START_BIN/install-k3s"
printf '%s\n' \
    '#!/bin/bash' \
    'exec bash "$CONFIGURE_CONTAINERD_SOURCE" "$@"' \
    >"$START_BIN/configure-containerd"
printf '%s\n' \
    '#!/bin/sh' \
    'case "$*" in' \
    '*"get node hamn"*) printf "hamn Ready\n" ;;' \
    '*"get pods"*) printf "10.42.0.2\n" ;;' \
    '*"rollout restart deployment/coredns"*)' \
    '    printf "restart\n" >>"$K3S_LOG"' \
    '    [ -z "${K3S_RESTART_FAILURE:-}" ] ;;' \
    '*"rollout status deployment/coredns"*)' \
    '    printf "status\n" >>"$K3S_LOG" ;;' \
    '*) exit 2 ;;' \
    'esac' >"$START_BIN/k3s"
printf '%s\n' '#!/bin/sh' 'printf "hamn NotReady\n"' \
    >"$START_BIN/not-ready-k3s"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$START_BIN/curl"
printf '%s\n' '#!/bin/sh' 'exit 22' >"$START_BIN/not-ready-curl"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$START_BIN/sleep"
chmod +x "$START_BIN/ctr" "$START_BIN/systemctl" \
    "$START_BIN/install-k3s" "$START_BIN/configure-containerd" \
    "$START_BIN/k3s" "$START_BIN/not-ready-k3s" "$START_BIN/curl" \
    "$START_BIN/not-ready-curl" "$START_BIN/sleep"
SYSTEMCTL_STATE="$WORK/systemctl.state"
K3S_LOG="$WORK/k3s.log"
export SYSTEMCTL_STATE K3S_LOG SNAPSHOT_ROOT
: >"$K3S_LOG"
printf '0 0\n' >"$SYSTEMCTL_STATE"
run_start() {
    TMPDIR="$SNAPSHOT_ROOT" PATH="$START_BIN:/usr/bin:/bin" \
    CONFIGURE_CONTAINERD_SOURCE="$ROOT/scripts/configure-containerd.sh" \
    HAMN_CONFIGURE_CONTAINERD="$START_BIN/configure-containerd" \
    HAMN_INSTALL_K3S="$START_BIN/install-k3s" HAMN_K3S_BIN="$2" \
    HAMN_CURL="$3" \
    HAMN_CNI_SOURCE_DIR="$START_SOURCE" HAMN_CNI_BIN_DIR="$START_DEST" \
    HAMN_K3S_CNI_CONFIG_SOURCE="$WORK/source/10-flannel.conflist" \
    HAMN_K3S_FLANNEL_SOURCE="$WORK/source/flannel" \
    HAMN_K3S_BANDWIDTH_SOURCE="$WORK/source/bandwidth" \
    HAMN_CNI_CONFIG_LINK="$1" \
    HAMN_FLANNEL_LINK="$WORK/opt/cni/bin/flannel" \
    HAMN_BANDWIDTH_LINK="$WORK/opt/cni/bin/bandwidth" \
    HAMN_K3S_CNI_TRANSACTION_DIR="$CNI_TRANSACTION" \
        bash "$ROOT/scripts/configure-k3s.sh" start
}
GOOD_LINK="$WORK/etc/cni/net.d/10-flannel.conflist"
run_start "$GOOD_LINK" "$START_BIN/k3s" "$START_BIN/curl"
[ -x "$START_DEST/bridge" ]
[ "$(readlink "$START_DEST/bridge")" = "$START_SOURCE/bridge" ]
test "$(grep -c '^restart$' "$K3S_LOG")" -eq 1
test "$(grep -c '^status$' "$K3S_LOG")" -eq 1
test -z "$(find "$SNAPSHOT_ROOT" -mindepth 1 -maxdepth 1 -print)"

# A failure after two replacements must restore a symlink, a regular file,
# and an absent path before restoring the previous service state.
OLD_CONFIG="$WORK/source/old.conflist"
printf '{"old":true}\n' >"$OLD_CONFIG"
ln -sfn "$OLD_CONFIG" "$GOOD_LINK"
rm -f "$WORK/opt/cni/bin/flannel" "$WORK/opt/cni/bin/bandwidth"
printf 'old flannel\n' >"$WORK/opt/cni/bin/flannel"
chmod 0640 "$WORK/opt/cni/bin/flannel"
printf '1 0\n' >"$SYSTEMCTL_STATE"
export HAMN_TEST=1 HAMN_TEST_CNI_LINK_FAILURE_AT=3
if run_start "$GOOD_LINK" "$START_BIN/k3s" "$START_BIN/curl" \
    2>"$WORK/cni-third-step.error"; then
    echo "FAIL: injected third CNI link failure was accepted" >&2
    exit 1
fi
unset HAMN_TEST HAMN_TEST_CNI_LINK_FAILURE_AT
grep -q 'injected CNI link failure at step 3' \
    "$WORK/cni-third-step.error"
test "$(cat "$SYSTEMCTL_STATE")" = "1 0"
test "$(readlink "$GOOD_LINK")" = "$OLD_CONFIG"
test ! -L "$WORK/opt/cni/bin/flannel"
test "$(cat "$WORK/opt/cni/bin/flannel")" = "old flannel"
test -n "$(find "$WORK/opt/cni/bin/flannel" -prune -perm 0640 -print)"
test ! -e "$WORK/opt/cni/bin/bandwidth"
test ! -L "$WORK/opt/cni/bin/bandwidth"
test -z "$(find "$SNAPSHOT_ROOT" -mindepth 1 -maxdepth 1 -print)"

repair_cni

printf '0 1\n' >"$SYSTEMCTL_STATE"
export SYSTEMCTL_QUERY_FAILURE=1
if run_start "$GOOD_LINK" "$START_BIN/k3s" "$START_BIN/curl" \
    2>"$WORK/state-start.error"; then
    echo "FAIL: uncertain k3s lifecycle state was accepted" >&2
    exit 1
fi
unset SYSTEMCTL_QUERY_FAILURE
grep -q 'cannot determine the current k3s service state' \
    "$WORK/state-start.error"
test "$(cat "$SYSTEMCTL_STATE")" = "0 1"

FAIL_LINK="$WORK/cni-destination-directory"
mkdir "$FAIL_LINK"
printf '0 1\n' >"$SYSTEMCTL_STATE"
if run_start "$FAIL_LINK" "$START_BIN/k3s" "$START_BIN/curl" \
    2>"$WORK/cni-start.error"; then
    echo "FAIL: invalid CNI destination was accepted" >&2
    exit 1
fi
grep -q 'CNI link destination is a directory' "$WORK/cni-start.error"
test "$(cat "$SYSTEMCTL_STATE")" = "0 1"

printf '1 0\n' >"$SYSTEMCTL_STATE"
if run_start "$GOOD_LINK" "$START_BIN/not-ready-k3s" "$START_BIN/curl" \
    2>"$WORK/ready-start.error"; then
    echo "FAIL: a NotReady k3s node was accepted" >&2
    exit 1
fi
grep -q 'k3s node did not become Ready' "$WORK/ready-start.error"
test "$(cat "$SYSTEMCTL_STATE")" = "1 0"

printf '0 1\n' >"$SYSTEMCTL_STATE"
if run_start "$GOOD_LINK" "$START_BIN/k3s" "$START_BIN/not-ready-curl" \
    2>"$WORK/dns-ready-start.error"; then
    echo "FAIL: unavailable cluster DNS was accepted" >&2
    exit 1
fi
grep -q 'k3s cluster DNS did not become ready' \
    "$WORK/dns-ready-start.error"
test "$(cat "$SYSTEMCTL_STATE")" = "0 1"

printf '0 0\n' >"$SYSTEMCTL_STATE"
export K3S_RESTART_FAILURE=1
if run_start "$GOOD_LINK" "$START_BIN/k3s" "$START_BIN/curl" \
    2>"$WORK/dns-refresh-start.error"; then
    echo "FAIL: a failed CoreDNS refresh was accepted" >&2
    exit 1
fi
unset K3S_RESTART_FAILURE
grep -q 'cannot refresh CoreDNS after k3s restart' \
    "$WORK/dns-refresh-start.error"
test "$(cat "$SYSTEMCTL_STATE")" = "0 0"
printf '0 1\n' >"$SYSTEMCTL_STATE"
PATH="$START_BIN:/usr/bin:/bin" \
    bash "$ROOT/scripts/configure-k3s.sh" restore-state 1 0
test "$(cat "$SYSTEMCTL_STATE")" = "1 0"
test "$(PATH="$START_BIN:/usr/bin:/bin" bash \
    "$ROOT/scripts/configure-k3s.sh" lifecycle-state)" = "active=1 enabled=0"

bash "$ROOT/tests/test_configure_containerd.sh"

echo "PASS: k3s is pinned to external CRI with non-overlapping CIDRs"
