#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

BIN=$WORK/bin
CNI=$WORK/cni
ETC=$WORK/etc/hamn
mkdir -p "$BIN" "$CNI" "$ETC"
for command in dockerd docker containerd ctr runc dnsmasq qemu-x86_64-static hamnd \
    getent systemctl; do
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$BIN/$command"
    chmod 0755 "$BIN/$command"
done
for plugin in bridge host-local loopback portmap firewall tuning; do
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$CNI/$plugin"
    chmod 0755 "$CNI/$plugin"
done
HAMND=$BIN/hamnd
BINFMT=$WORK/qemu-x86_64
printf 'enabled\n' >"$BINFMT"
ROSETTA_BINFMT=$WORK/hamn-rosetta
printf 'enabled\n' >"$ROSETTA_BINFMT"
MANIFEST=$ETC/guest-image.json
K3S=$ETC/k3s-compatibility.json
KEY=$ETC/hamn-release.pub
printf '%s' \
    '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64",' \
    '"components":["docker","buildkit","containerd","runc","cni","binfmt","dnsmasq","hamnd"]}' \
    >"$MANIFEST"
printf '{}\n' >"$K3S"
printf 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGuestImageContractTestKey hamn-release\n' >"$KEY"
printf 'signature fixture\n' >"$K3S.sig"

run_contract() {
    local mode=${1:-qemu}
    local entry=${2:-$BINFMT}
    PATH="$BIN:/usr/bin:/bin" \
    HAMN_GUEST_ARCH=aarch64 \
    HAMN_GUEST_IMAGE_MANIFEST="$MANIFEST" \
    HAMN_K3S_MANIFEST="$K3S" \
    HAMN_K3S_MANIFEST_SIGNATURE="$K3S.sig" \
    HAMN_K3S_PUBLIC_KEY="$KEY" \
    HAMN_CNI_SOURCE_DIR="$CNI" \
    HAMN_HAMND_BIN="$HAMND" \
    HAMN_BINFMT_MODE="$mode" \
    HAMN_BINFMT_ENTRY="$entry" \
        bash "$ROOT/scripts/verify-image-contract.sh"
}

run_contract >"$WORK/valid.out"
grep -Fq 'verified Ubuntu 24.04 arm64 guest image contract' "$WORK/valid.out"
run_contract rosetta "$ROSETTA_BINFMT" >"$WORK/rosetta.out"

printf '%s\n' \
    '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64",' \
    '"components":["docker","containerd","runc","cni","binfmt","dnsmasq","hamnd"]}' \
    >"$MANIFEST"
if run_contract >"$WORK/missing-buildkit.out" 2>"$WORK/missing-buildkit.err"; then
    echo "FAIL: image contract accepted a manifest without embedded BuildKit" >&2
    exit 1
fi
grep -Fq 'component set is invalid' "$WORK/missing-buildkit.err"

printf '%s' \
    '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64",' \
    '"components":["docker","buildkit","containerd","runc","cni","binfmt","dnsmasq","hamnd"]}' \
    >"$MANIFEST"

if run_contract invalid "$BINFMT" >"$WORK/invalid-mode.out" \
    2>"$WORK/invalid-mode.err"; then
    echo "FAIL: image contract accepted an invalid binfmt mode" >&2
    exit 1
fi
grep -Fq 'invalid binfmt mode' "$WORK/invalid-mode.err"

rm "$CNI/bridge"
if run_contract >"$WORK/missing.out" 2>"$WORK/missing.err"; then
    echo "FAIL: image contract accepted a missing CNI component" >&2
    exit 1
fi
grep -Fq 'required CNI plugin is unavailable: bridge' "$WORK/missing.err"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$CNI/bridge"
chmod 0755 "$CNI/bridge"

printf '%s\n' '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":[]}' >"$MANIFEST"
if run_contract >"$WORK/malformed.out" 2>"$WORK/malformed.err"; then
    echo "FAIL: image contract accepted a malformed component set" >&2
    exit 1
fi
grep -Fq 'component set is invalid' "$WORK/malformed.err"

echo "PASS: preconfigured guest image contract is strict and fail-closed"
