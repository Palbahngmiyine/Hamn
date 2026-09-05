#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

make -C "$ROOT" -B -n install >"$WORK/install"
grep -q 'build/hamnd-agent' "$WORK/install"
grep -q 'configure-containerd' "$WORK/install"
grep -q 'configure-docker' "$WORK/install"
grep -q 'configure-rosetta' "$WORK/install"
grep -q -- '-pthread' "$WORK/install"
grep -q 'verify-image-contract' "$WORK/install"
if grep -Eq 'k3s|hamn-engine|nerdctl|managed-kind|kind-provider|ip_reporter|guest_ip_report' "$WORK/install"; then
    echo "FAIL: Docker-only guest install retains a legacy runtime artifact" >&2
    exit 1
fi

echo "PASS: guest install preserves Docker/CRI without managed K3s helpers"
