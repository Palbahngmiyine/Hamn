#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

# The JSON helper and socket fixture are C programs; a caller may supply
# prebuilt (for example sanitizer-instrumented) ones.
GUEST_JSON=${HAMN_TEST_GUEST_JSON:-}
FIXTURE=${HAMN_TEST_GUEST_FIXTURE:-}
if [ -z "$GUEST_JSON" ] || [ -z "$FIXTURE" ]; then
    make -s --no-print-directory -C "$ROOT" build/guest-json build/guest-test-fixture
    GUEST_JSON=${GUEST_JSON:-$ROOT/build/guest-json}
    FIXTURE=${FIXTURE:-$ROOT/build/guest-test-fixture}
fi

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
# Guest configuration must never need an interpreter: a python3 lookup
# records itself and fails.
cat >"$BIN/python3" <<'EOF'
#!/bin/sh
: >"$HAMN_TEST_STATE/python3-invoked"
exit 97
EOF
chmod +x "$BIN"/*
export PATH="$BIN:$PATH"

# configure-docker requires only a Unix socket file at the containerd path.
SOCKET="$WORK/containerd.sock"
"$FIXTURE" bind-unix "$SOCKET"
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
export HAMN_GUEST_JSON="$GUEST_JSON"

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
grep -Fq "ExecStart=$BIN/dnsmasq --keep-in-foreground --conf-file=$HAMN_HOST_DNS_CONFIG" \
    "$HAMN_HOST_DNS_UNIT"
grep -Fxq "ExecStart=$BIN/dockerd -H fd://" \
    "$HAMN_DOCKER_DROPIN"
# The 0.0.1 host.hamn.internal alias is gone: the DNS configuration names only
# host.docker.internal, and success prints no deprecation warning.
cat >"$WORK/expected-dns.conf" <<'EOF'
bind-dynamic
listen-address=172.17.0.1
no-hosts
address=/host.docker.internal/192.168.64.1
EOF
cmp "$WORK/expected-dns.conf" "$HAMN_HOST_DNS_CONFIG"
[ ! -s "$WORK/first.err" ]

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
# The whole file is sorted, two-space indented JSON with a final newline.
cat >"$WORK/expected-daemon.json" <<EOF
{
  "bip": "172.17.0.1/16",
  "containerd": "$SOCKET",
  "debug": true,
  "dns": [
    "172.17.0.1"
  ],
  "features": {
    "buildkit": true,
    "containerd-snapshotter": true
  },
  "host-gateway-ip": "192.168.64.1"
}
EOF
cmp "$WORK/expected-daemon.json" "$HAMN_DOCKER_CONFIG"

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
! grep -Fq 'host.hamn.internal' "$HAMN_HOST_DNS_CONFIG"

# Values keep the established daemon.json form: \u escapes, exact integers,
# shortest float repr, and an explicitly true BuildKit feature.
HAMN_TEST_GATEWAY=192.168.64.2 \
HAMN_DOCKER_EXTRA_JSON='{"labels":["zone=é","q\"t"],"max-concurrent-downloads":3,"x-ratio":1.50,"features":{"buildkit":true}}' \
    bash "$ROOT/scripts/configure-docker.sh" 2>/dev/null
cat >"$WORK/expected-escaped.json" <<EOF
{
  "bip": "172.17.0.1/16",
  "containerd": "$SOCKET",
  "dns": [
    "172.17.0.1"
  ],
  "features": {
    "buildkit": true
  },
  "host-gateway-ip": "192.168.64.2",
  "labels": [
    "zone=\\u00e9",
    "q\\"t"
  ],
  "max-concurrent-downloads": 3,
  "x-ratio": 1.5
}
EOF
cmp "$WORK/expected-escaped.json" "$HAMN_DOCKER_CONFIG"

# Every rejected setting fails with its own reason and keeps the active
# valid configuration byte for byte.
cp "$HAMN_DOCKER_CONFIG" "$WORK/daemon.before.json"
reject() {
    local extra=$1 reason=$2
    if HAMN_TEST_GATEWAY=192.168.64.2 HAMN_DOCKER_EXTRA_JSON="$extra" \
        bash "$ROOT/scripts/configure-docker.sh" \
        >"$WORK/invalid.out" 2>"$WORK/invalid.err"; then
        echo "FAIL: unsafe Docker daemon settings were accepted: $extra" >&2
        exit 1
    fi
    grep -Fq -- "$reason" "$WORK/invalid.err" || {
        echo "FAIL: $extra was rejected without '$reason':" >&2
        cat "$WORK/invalid.err" >&2
        exit 1
    }
    grep -Fq 'cannot validate Docker daemon settings' "$WORK/invalid.err"
    cmp "$HAMN_DOCKER_CONFIG" "$WORK/daemon.before.json"
}
for managed in containerd host-gateway-ip hosts data-root exec-root dns bip \
    bridge fixed-cidr default-address-pools; do
    reject "{\"$managed\":null}" "cannot override Hamn-managed key: $managed"
done
reject '{"hosts":["tcp://0.0.0.0:2375"]}' 'cannot override Hamn-managed key: hosts'
reject '{"features":{"buildkit":false}}' 'features.buildkit must remain true'
reject '{"features":{"buildkit":1}}' 'features.buildkit must remain true'
reject '{"features":[]}' 'docker.daemonJson.features must be a JSON object'
reject '[]' 'docker.daemonJson must be one JSON object'
reject '"debug"' 'docker.daemonJson must be one JSON object'
reject '{"debug":true,"debug":false}' 'must be one strict JSON object: duplicate key: debug'
reject '{"log-opts":{"a":"1","a":"2"}}' 'must be one strict JSON object: duplicate key: a'
reject '{' 'docker.daemonJson must be one strict JSON object'
reject '{"debug":NaN}' 'docker.daemonJson must be one strict JSON object'
reject '{"debug":-Infinity}' 'docker.daemonJson must be one strict JSON object'
reject '{"x":1e400}' 'docker.daemonJson must be one strict JSON object'
reject '{"debug":true} {}' 'docker.daemonJson must be one strict JSON object'
reject ' ' 'docker.daemonJson must be one strict JSON object'

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

# A missing JSON helper is the same kind of loud image-contract failure.
if HAMN_GUEST_JSON="$BIN/does-not-exist" HAMN_DOCKER_CONFIG="$MISSING_CONFIG" \
    bash "$ROOT/scripts/configure-docker.sh" >"$WORK/missing.out" \
    2>"$WORK/missing.err"; then
    echo "FAIL: configure-docker accepted a missing guest-json helper" >&2
    exit 1
fi
grep -Fq 'guest-json is missing from the guest image' "$WORK/missing.err"
test ! -e "$MISSING_CONFIG"
test ! -e "$STATE/python3-invoked"

echo "PASS: dockerd is pinned to system containerd and configured atomically"
