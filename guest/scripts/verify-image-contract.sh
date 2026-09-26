#!/bin/bash
set -euo pipefail
export LC_ALL=C

# This check is intentionally local and side-effect free. It distinguishes a
# signed preconfigured Hamn image from a stock Ubuntu cloud image before host
# lifecycle code writes containerd, or Docker state into the guest.
IMAGE_MANIFEST=${HAMN_GUEST_IMAGE_MANIFEST:-/etc/hamn/guest-image.json}
CNI_DIR=${HAMN_CNI_SOURCE_DIR:-/usr/lib/cni}
HAMND_BIN=${HAMN_HAMND_BIN:-/usr/local/bin/hamnd}
GETENT=${HAMN_GETENT:-getent}
SYSTEMCTL=${HAMN_SYSTEMCTL:-systemctl}
GUEST_JSON=${HAMN_GUEST_JSON:-/usr/local/libexec/hamn/guest-json}
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

[ -f "$GUEST_JSON" ] && [ -x "$GUEST_JSON" ] ||
    fail "required image component is unavailable: guest-json"
# Exact schema, distribution, architecture and component set; duplicate keys
# and non-JSON input fail with the manifest error guest-json prints.
"$GUEST_JSON" image-manifest "$IMAGE_MANIFEST" || exit 1

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
