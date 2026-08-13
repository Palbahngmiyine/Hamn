#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
SOCKET_PID=
cleanup() {
    if [ -n "$SOCKET_PID" ]; then
        kill "$SOCKET_PID" 2>/dev/null || true
        wait "$SOCKET_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

BIN="$WORK/bin"
STATE="$WORK/state"
ETC="$WORK/etc"
mkdir -p "$BIN" "$STATE" "$ETC"
LOG="$WORK/systemctl.log"

cat >"$BIN/systemctl" <<'EOF'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >>"$HAMN_TEST_SYSTEMCTL_LOG"
action=$1
service=${3:-${2:-}}
case "$action" in
is-enabled) [ -f "$HAMN_TEST_STATE/enabled-$service" ] ;;
is-active) [ -f "$HAMN_TEST_STATE/active-$service" ] ;;
enable) touch "$HAMN_TEST_STATE/enabled-$service" ;;
start|restart) touch "$HAMN_TEST_STATE/active-$service" ;;
daemon-reload) ;;
*) exit 2 ;;
esac
EOF
cat >"$BIN/dockerd" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$BIN/docker" <<'EOF'
#!/bin/bash
set -euo pipefail
[ "$1" = version ]
[ -f "$HAMN_TEST_STATE/active-docker.service" ]
printf '26.0.0\n'
EOF
cat >"$BIN/dnsmasq" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$BIN/ip" <<'EOF'
#!/bin/sh
printf 'default via %s dev eth0\n' "${HAMN_TEST_GATEWAY:-192.168.64.1}"
EOF
chmod +x "$BIN"/*

SOCKET="$WORK/containerd.sock"
python3 - "$SOCKET" <<'PY' &
import os
import socket
import sys
path = sys.argv[1]
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(path)
server.listen(1)
try:
    while True:
        connection, _ = server.accept()
        connection.close()
finally:
    server.close()
    os.unlink(path)
PY
SOCKET_PID=$!
for _ in $(seq 1 50); do
    [ -S "$SOCKET" ] && break
    sleep 0.02
done
[ -S "$SOCKET" ] || { echo "FAIL: test containerd socket missing" >&2; exit 1; }

export HAMN_SYSTEMCTL="$BIN/systemctl"
export HAMN_TEST_SYSTEMCTL_LOG="$LOG"
export HAMN_TEST_STATE="$STATE"
export HAMN_DOCKERD="$BIN/dockerd"
export HAMN_DOCKER="$BIN/docker"
export HAMN_DNSMASQ="$BIN/dnsmasq"
export HAMN_IP="$BIN/ip"
export HAMN_CONTAINERD_SOCKET="$SOCKET"
export HAMN_DOCKER_CONFIG="$ETC/docker/daemon.json"
export HAMN_DOCKER_DROPIN_DIR="$ETC/systemd/docker.service.d"
export HAMN_DOCKER_DROPIN="$HAMN_DOCKER_DROPIN_DIR/10-hamn-containerd.conf"
export HAMN_HOST_DNS_CONFIG="$ETC/dnsmasq.d/hamn-host-dns.conf"
export HAMN_HOST_DNS_UNIT="$ETC/systemd/hamn-host-dns.service"

: >"$LOG"
bash "$ROOT/scripts/configure-docker.sh" >"$WORK/first.out" 2>"$WORK/first.err"
grep -Fxq 'daemon-reload' "$LOG"
grep -Fxq 'enable hamn-host-dns.service' "$LOG"
grep -Fxq 'start hamn-host-dns.service' "$LOG"
grep -Fxq 'enable docker.service' "$LOG"
grep -Fxq 'start docker.service' "$LOG"
grep -Fq "\"containerd\": \"$SOCKET\"" "$HAMN_DOCKER_CONFIG"
grep -Fq '"host-gateway-ip": "192.168.64.1"' "$HAMN_DOCKER_CONFIG"
grep -Fq '"bip": "172.17.0.1/16"' "$HAMN_DOCKER_CONFIG"
grep -Fq '"dns": [' "$HAMN_DOCKER_CONFIG"
grep -Fxq 'listen-address=172.17.0.1' "$HAMN_HOST_DNS_CONFIG"
grep -Fxq 'address=/host.docker.internal/192.168.64.1' "$HAMN_HOST_DNS_CONFIG"
grep -Fxq 'address=/host.hamn.internal/192.168.64.1' "$HAMN_HOST_DNS_CONFIG"
grep -Fq "ExecStart=$BIN/dnsmasq --keep-in-foreground --conf-file=$HAMN_HOST_DNS_CONFIG" \
    "$HAMN_HOST_DNS_UNIT"
grep -Fq "ExecStart=$BIN/dockerd -H fd:// --containerd=$SOCKET" \
    "$HAMN_DOCKER_DROPIN"
grep -Fq 'host.hamn.internal is a 0.0.1 compatibility alias' "$WORK/first.err"

# User daemon settings are merged without allowing a profile to replace
# Hamn's system containerd, Docker socket activation, host gateway, or DNS.
: >"$LOG"
HAMN_DOCKER_EXTRA_JSON='{"debug":true,"features":{"containerd-snapshotter":true}}' \
    bash "$ROOT/scripts/configure-docker.sh"
grep -Fq '"debug": true' "$HAMN_DOCKER_CONFIG"
grep -Fq '"buildkit": true' "$HAMN_DOCKER_CONFIG"
grep -Fq '"containerd-snapshotter": true' "$HAMN_DOCKER_CONFIG"
grep -Fq "\"containerd\": \"$SOCKET\"" "$HAMN_DOCKER_CONFIG"
grep -Fxq 'restart docker.service' "$LOG"

# Unchanged Docker settings do not churn systemd.
: >"$LOG"
HAMN_DOCKER_EXTRA_JSON='{"debug":true,"features":{"containerd-snapshotter":true}}' \
    bash "$ROOT/scripts/configure-docker.sh"
! grep -Eq '^(daemon-reload|enable|start|restart)' "$LOG"

# A changed gateway refreshes the host-name DNS mapping and Docker daemon.
: >"$LOG"
HAMN_TEST_GATEWAY=192.168.64.2 \
HAMN_DOCKER_EXTRA_JSON='{"debug":true,"features":{"containerd-snapshotter":true}}' \
    bash "$ROOT/scripts/configure-docker.sh"
grep -Fxq 'restart docker.service' "$LOG"
grep -Fxq 'restart hamn-host-dns.service' "$LOG"
grep -Fq '"host-gateway-ip": "192.168.64.2"' "$HAMN_DOCKER_CONFIG"
grep -Fxq 'address=/host.docker.internal/192.168.64.2' "$HAMN_HOST_DNS_CONFIG"

# Managed daemon keys fail without replacing the active valid configuration.
cp "$HAMN_DOCKER_CONFIG" "$WORK/daemon.before.json"
for extra in \
    '{"containerd":"/other.sock"}' \
    '{"hosts":["tcp://0.0.0.0:2375"]}' \
    '{"dns":["1.1.1.1"]}' \
    '{"bip":"10.0.0.1/24"}' \
    '{"features":{"buildkit":false}}' \
    '{"debug":true,"debug":false}' \
    '[]' \
    '{'; do
    if HAMN_DOCKER_EXTRA_JSON="$extra" bash "$ROOT/scripts/configure-docker.sh" \
        >"$WORK/invalid.out" 2>"$WORK/invalid.err"; then
        echo "FAIL: unsafe Docker daemon settings were accepted: $extra" >&2
        exit 1
    fi
    cmp "$HAMN_DOCKER_CONFIG" "$WORK/daemon.before.json"
done
grep -Fq 'cannot override Hamn-managed key' "$WORK/invalid.err" ||
    grep -Fq 'docker.daemonJson must be one strict JSON object' "$WORK/invalid.err" ||
    grep -Fq 'docker.daemonJson must be one JSON object' "$WORK/invalid.err" ||
    grep -Fq 'buildkit must remain true' "$WORK/invalid.err"

# Missing Docker Engine is a loud image-contract failure before a config write.
MISSING_CONFIG="$ETC/missing/daemon.json"
if HAMN_DOCKERD="$BIN/does-not-exist" HAMN_DOCKER_CONFIG="$MISSING_CONFIG" \
    bash "$ROOT/scripts/configure-docker.sh" >"$WORK/missing.out" \
    2>"$WORK/missing.err"; then
    echo "FAIL: configure-docker accepted a missing Docker Engine" >&2
    exit 1
fi
grep -Fq 'Docker Engine is missing from the guest image' "$WORK/missing.err"
test ! -e "$MISSING_CONFIG"

echo "PASS: dockerd is pinned to system containerd and configured atomically"
