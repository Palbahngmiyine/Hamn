#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

# A caller may supply a prebuilt (for example sanitizer-instrumented) helper.
GUEST_JSON=${HAMN_TEST_GUEST_JSON:-}
if [ -z "$GUEST_JSON" ]; then
    make -s --no-print-directory -C "$ROOT" build/guest-json
    GUEST_JSON=$ROOT/build/guest-json
fi

BIN=$WORK/bin
CNI=$WORK/cni
ETC=$WORK/etc/hamn
mkdir -p "$BIN" "$CNI" "$ETC"
for command in dockerd docker containerd ctr runc dnsmasq qemu-x86_64-static hamnd \
    getent systemctl; do
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$BIN/$command"
    chmod 0755 "$BIN/$command"
done
# The contract never needs an interpreter; a python3 lookup records itself.
printf '%s\n' '#!/bin/sh' ": >\"$WORK/python3-invoked\"" 'exit 97' >"$BIN/python3"
chmod 0755 "$BIN/python3"
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
VALID_MANIFEST='{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":["docker","buildkit","containerd","runc","cni","binfmt","dnsmasq","hamnd"]}'
printf '%s' "$VALID_MANIFEST" >"$MANIFEST"

run_contract() {
    local mode=${1:-qemu}
    local entry=${2:-$BINFMT}
    PATH="$BIN:/usr/bin:/bin" \
    HAMN_GUEST_ARCH=aarch64 \
    HAMN_GUEST_IMAGE_MANIFEST="$MANIFEST" \
    HAMN_CNI_SOURCE_DIR="$CNI" \
    HAMN_HAMND_BIN="$HAMND" \
    HAMN_GUEST_JSON="$GUEST_JSON" \
    HAMN_BINFMT_MODE="$mode" \
    HAMN_BINFMT_ENTRY="$entry" \
        bash "$ROOT/scripts/verify-image-contract.sh"
}

run_contract >"$WORK/valid.out"
grep -Fq 'verified Ubuntu 24.04 arm64 guest image contract' "$WORK/valid.out"
run_contract rosetta "$ROSETTA_BINFMT" >"$WORK/rosetta.out"
# Formatting and component order are not part of the contract.
printf '%s\n' '{ "components": ["hamnd","dnsmasq","binfmt","cni","runc",' \
    '"containerd","buildkit","docker"], "architecture": "arm64",' \
    '"distribution": "ubuntu-24.04", "schemaVersion": 1 }' >"$MANIFEST"
run_contract >"$WORK/reordered.out"

# Every manifest rejection names its reason under one stable prefix.
reject_manifest() {
    local manifest=$1 reason=$2
    printf '%s' "$manifest" >"$MANIFEST"
    if run_contract >"$WORK/manifest.out" 2>"$WORK/manifest.err"; then
        echo "FAIL: image contract accepted manifest: $manifest" >&2
        exit 1
    fi
    grep -Fq "hamn: guest image contract: invalid guest image manifest: $reason" \
        "$WORK/manifest.err" || {
        echo "FAIL: manifest was rejected without '$reason': $manifest" >&2
        cat "$WORK/manifest.err" >&2
        exit 1
    }
}
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":["docker","containerd","runc","cni","binfmt","dnsmasq","hamnd"]}' \
    'component set is invalid'
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":[]}' \
    'component set is invalid'
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":["docker","docker","containerd","runc","cni","binfmt","dnsmasq","hamnd"]}' \
    'component set is invalid'
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":["docker","buildkit","containerd","runc","cni","binfmt","dnsmasq",7]}' \
    'component set is invalid'
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":"docker"}' \
    'component set is invalid'
reject_manifest '{"schemaVersion":2,"distribution":"ubuntu-24.04","architecture":"arm64","components":[]}' \
    'schemaVersion is invalid'
reject_manifest '{"schemaVersion":"1","distribution":"ubuntu-24.04","architecture":"arm64","components":[]}' \
    'schemaVersion is invalid'
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-22.04","architecture":"arm64","components":[]}' \
    'distribution is invalid'
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"amd64","components":[]}' \
    'architecture is invalid'
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64"}' \
    'schema is invalid'
reject_manifest '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":[],"extra":true}' \
    'schema is invalid'
reject_manifest '["schemaVersion","distribution","architecture","components"]' \
    'schema is invalid'
reject_manifest '{"schemaVersion":1,"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64","components":[]}' \
    'duplicate key: schemaVersion'
reject_manifest '{"schemaVersion":NaN,"distribution":"ubuntu-24.04","architecture":"arm64","components":[]}' \
    'invalid JSON'
reject_manifest '{"schemaVersion":1' 'invalid JSON'
reject_manifest '' 'invalid JSON'
printf '%s' "$VALID_MANIFEST" >"$MANIFEST"

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

if PATH="$BIN:/usr/bin:/bin" HAMN_GUEST_ARCH=aarch64 \
    HAMN_GUEST_IMAGE_MANIFEST="$MANIFEST" HAMN_CNI_SOURCE_DIR="$CNI" \
    HAMN_HAMND_BIN="$HAMND" HAMN_GUEST_JSON="$WORK/missing-guest-json" \
    HAMN_BINFMT_ENTRY="$BINFMT" bash "$ROOT/scripts/verify-image-contract.sh" \
    >"$WORK/no-helper.out" 2>"$WORK/no-helper.err"; then
    echo "FAIL: image contract accepted a missing guest-json helper" >&2
    exit 1
fi
grep -Fq 'required image component is unavailable: guest-json' "$WORK/no-helper.err"
test ! -e "$WORK/python3-invoked"

echo "PASS: preconfigured guest image contract is strict and fail-closed"
