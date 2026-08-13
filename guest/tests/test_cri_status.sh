#!/bin/bash
set -euo pipefail

TEST_BIN=$1
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
CTR=$WORK/ctr

cat >"$CTR" <<'EOF'
#!/bin/sh
[ "$#" -eq 4 ] && [ "$1" = --address ] &&
    [ "$2" = /run/containerd/containerd.sock ] &&
    [ "$3" = plugins ] && [ "$4" = ls ] || exit 91
case "${HAMN_TEST_CRI_MODE:-}" in
healthy) printf '%s\n' 'io.containerd.grpc.v1 cri linux/arm64 ok' ;;
alternate) printf '%s\n' 'io.containerd.grpc.v1.cri runtime linux/arm64 ok' ;;
containerd2) printf '%s\n' 'io.containerd.cri.v1 runtime linux/arm64 ok' ;;
unhealthy) printf '%s\n' 'io.containerd.grpc.v1 cri linux/arm64 error' ;;
missing) printf '%s\n' 'io.containerd.grpc.v1 tasks linux/arm64 ok' ;;
failure) exit 7 ;;
timeout) while :; do :; done ;;
*) exit 92 ;;
esac
EOF
chmod 0755 "$CTR"

HAMN_TEST_CRI_MODE=healthy "$TEST_BIN" "$CTR" 1000 true
HAMN_TEST_CRI_MODE=alternate "$TEST_BIN" "$CTR" 1000 true
HAMN_TEST_CRI_MODE=containerd2 "$TEST_BIN" "$CTR" 1000 true
HAMN_TEST_CRI_MODE=unhealthy "$TEST_BIN" "$CTR" 1000 false
HAMN_TEST_CRI_MODE=missing "$TEST_BIN" "$CTR" 1000 false
HAMN_TEST_CRI_MODE=failure "$TEST_BIN" "$CTR" 1000 false
"$TEST_BIN" "$WORK/missing-ctr" 1000 false
HAMN_TEST_CRI_MODE=timeout "$TEST_BIN" "$CTR" 50 false

ROOT=$(cd "$(dirname "$0")/.." && pwd)
grep -Fq '#define CTR_PATH "/usr/bin/ctr"' "$ROOT/agent/api/cri_status.c"
if grep -Eq 'socket\(|connect\(' "$ROOT/agent/api/cri_status.c"; then
    echo "FAIL: CRI readiness regressed to a socket-connect probe" >&2
    exit 1
fi
grep -Fq 'cJSON_AddBoolToObject(j, "criReady", cri_plugin_ready());' \
    "$ROOT/agent/api/router.c"

echo "PASS: CRI readiness requires a healthy system containerd plugin"
