#!/bin/bash
# Single-profile K3s CLI checks that do not start a VM.
set -euo pipefail

HAMN="${HAMN:-build/hamn}"
WORK=$(mktemp -d /tmp/hamn-kubernetes-profile.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

run_hamn() {
    HOME="$WORK" "$HAMN" "$@"
}

run_hamn kubernetes status >"$WORK/default.out"
grep -Fxq disabled "$WORK/default.out"
HOME="$WORK" HAMN_PROFILE=from-env "$HAMN" kubernetes status >"$WORK/env.out"
grep -Fxq disabled "$WORK/env.out"
run_hamn kubernetes status -p from-flag from-positional >"$WORK/flag.out"
grep -Fxq disabled "$WORK/flag.out"

mkdir -p "$WORK/.hamn/enabled"
printf '%s\n' 'cpus: 4' 'memoryMiB: 4096' 'diskGiB: 60' 'kubernetes:' '  enabled: true' '  version: "v1.36.2+k3s1"' >"$WORK/.hamn/enabled/config.yaml"
run_hamn kubernetes status enabled >"$WORK/enabled.out"
grep -Fxq stopped "$WORK/enabled.out"

if run_hamn kubernetes contexts >"$WORK/catalog.out" 2>"$WORK/catalog.err"; then
    echo "FAIL: external Kubernetes catalog command remains public" >&2
    exit 1
fi
grep -q 'usage: hamn kubernetes' "$WORK/catalog.err"
if run_hamn kubernetes create kind >"$WORK/kind.out" 2>"$WORK/kind.err"; then
    echo "FAIL: managed kind command remains public" >&2
    exit 1
fi
grep -q 'usage: hamn kubernetes' "$WORK/kind.err"
if run_hamn kubernetes start enabled >"$WORK/start.out" 2>"$WORK/start.err"; then
    echo "FAIL: Kubernetes start accepted a stopped VM" >&2
    exit 1
fi
grep -q 'is not running' "$WORK/start.err"
if run_hamn kubectl enabled -- get nodes >"$WORK/kubectl.out" 2>"$WORK/kubectl.err"; then
    echo "FAIL: kubectl accepted a missing profile kubeconfig" >&2
    exit 1
fi
grep -q 'kubeconfig is not ready' "$WORK/kubectl.err"
if run_hamn kubectl --connection foreign get nodes >"$WORK/connection.out" 2>"$WORK/connection.err"; then
    echo "FAIL: external kubectl connection remains public" >&2
    exit 1
fi
grep -q 'Kubernetes is disabled' "$WORK/connection.err"

echo "PASS: K3s is the only Kubernetes CLI surface"
