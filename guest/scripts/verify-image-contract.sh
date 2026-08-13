#!/bin/bash
set -euo pipefail
export LC_ALL=C

# This check is intentionally local and side-effect free. It distinguishes a
# signed preconfigured Hamn image from a stock Ubuntu cloud image before host
# lifecycle code writes containerd, Docker, or K3s state into the guest.
IMAGE_MANIFEST=${HAMN_GUEST_IMAGE_MANIFEST:-/etc/hamn/guest-image.json}
K3S_MANIFEST=${HAMN_K3S_MANIFEST:-/etc/hamn/k3s-compatibility.json}
K3S_SIGNATURE=${HAMN_K3S_MANIFEST_SIGNATURE:-$K3S_MANIFEST.sig}
RELEASE_KEY=${HAMN_K3S_PUBLIC_KEY:-/etc/hamn/hamn-release.pub}
CNI_DIR=${HAMN_CNI_SOURCE_DIR:-/usr/lib/cni}
HAMND_BIN=${HAMN_HAMND_BIN:-/usr/local/bin/hamnd}
GETENT=${HAMN_GETENT:-getent}
SYSTEMCTL=${HAMN_SYSTEMCTL:-systemctl}
BINFMT_MODE=${HAMN_BINFMT_MODE:-qemu}
case "$BINFMT_MODE" in
    qemu) DEFAULT_BINFMT_ENTRY=/proc/sys/fs/binfmt_misc/qemu-x86_64 ;;
    rosetta) DEFAULT_BINFMT_ENTRY=/proc/sys/fs/binfmt_misc/hamn-rosetta ;;
    *) echo "hamn: guest image contract: invalid binfmt mode: $BINFMT_MODE" >&2; exit 1 ;;
esac
BINFMT_ENTRY=${HAMN_BINFMT_ENTRY:-$DEFAULT_BINFMT_ENTRY}
ARCH=${HAMN_GUEST_ARCH:-$(uname -m)}

fail() {
    echo "hamn: guest image contract: $*" >&2
    exit 1
}

safe_regular() {
    [ -f "$1" ] && [ ! -L "$1" ]
}

[ "$ARCH" = aarch64 ] || fail "guest architecture is not arm64"
safe_regular "$IMAGE_MANIFEST" || fail "guest image manifest is unavailable"
safe_regular "$K3S_MANIFEST" || fail "K3s compatibility manifest is unavailable"
safe_regular "$K3S_SIGNATURE" || fail "K3s compatibility manifest signature is unavailable"
safe_regular "$RELEASE_KEY" || fail "release public key is unavailable"

python3 - "$IMAGE_MANIFEST" <<'PY'
import json
import sys


def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise ValueError("duplicate key: " + key)
        result[key] = value
    return result


try:
    with open(sys.argv[1], encoding="utf-8") as source:
        value = json.load(source, object_pairs_hook=pairs,
                          parse_constant=lambda text: (_ for _ in ()).throw(ValueError(text)))
    expected = {
        "schemaVersion": 1,
        "distribution": "ubuntu-24.04",
        "architecture": "arm64",
    }
    if not isinstance(value, dict) or set(value) != set(expected) | {"components"}:
        raise ValueError("schema is invalid")
    for key, expected_value in expected.items():
        if value[key] != expected_value:
            raise ValueError(key + " is invalid")
    components = value["components"]
    required = {"docker", "buildkit", "containerd", "runc", "cni", "binfmt", "dnsmasq", "hamnd"}
    if (not isinstance(components, list) or
            len(components) != len(required) or
            any(not isinstance(component, str) for component in components) or
            set(components) != required):
        raise ValueError("component set is invalid")
except (OSError, TypeError, ValueError, json.JSONDecodeError) as error:
    raise SystemExit("hamn: guest image contract: invalid guest image manifest: " + str(error))
PY

# Docker's default Buildx driver uses BuildKit server components embedded in
# dockerd. The contract intentionally attests that guest capability without
# requiring a host CLI plugin, standalone buildkitd, or public BuildKit socket.
for command in dockerd docker containerd ctr runc dnsmasq qemu-x86_64-static; do
    command -v "$command" >/dev/null 2>&1 ||
        fail "required image component is unavailable: $command"
done
for plugin in bridge host-local loopback portmap firewall tuning; do
    [ -x "$CNI_DIR/$plugin" ] ||
        fail "required CNI plugin is unavailable: $plugin"
done
[ -x "$HAMND_BIN" ] || fail "required image component is unavailable: hamnd"
"$GETENT" group hamn >/dev/null 2>&1 ||
    fail "required guest group is unavailable: hamn"
"$SYSTEMCTL" is-enabled --quiet hamnd.service >/dev/null 2>&1 ||
    fail "hamnd.service is not enabled in the guest image"
[ -f "$BINFMT_ENTRY" ] && grep -Fxq enabled "$BINFMT_ENTRY" ||
    fail "amd64 binfmt registration is unavailable"

echo "hamn: verified Ubuntu 24.04 arm64 guest image contract"
