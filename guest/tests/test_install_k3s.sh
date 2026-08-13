#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

command -v ssh-keygen >/dev/null || {
    echo "SKIP: ssh-keygen is unavailable" >&2
    exit 0
}

VERSION=v1.36.2+k3s1
BIN_SOURCE=$WORK/k3s-arm64
AIRGAP_SOURCE=$WORK/k3s-airgap-images-arm64.tar.zst
DEST=$WORK/bin/k3s
AIRGAP_DIR=$WORK/images
MANIFEST=$WORK/k3s-compatibility.json
KEY=$WORK/release-key

mkdir -p "$WORK/bin" "$AIRGAP_DIR"
printf '%s\n' '#!/bin/sh' "printf 'k3s version $VERSION\\n'" >"$BIN_SOURCE"
chmod 0755 "$BIN_SOURCE"
printf 'signed air-gap images\n' >"$AIRGAP_SOURCE"
binary_hash=$(sha256sum "$BIN_SOURCE" | awk '{print $1}')
airgap_hash=$(sha256sum "$AIRGAP_SOURCE" | awk '{print $1}')
printf '%s' \
    '{"schemaVersion":1,"version":"' "$VERSION" '",' \
    '"architecture":"arm64",' \
    '"binary":{"url":"file://' "$BIN_SOURCE" '","sha256":"' "$binary_hash" '"},' \
    '"airgapImages":{"url":"file://' "$AIRGAP_SOURCE" '","sha256":"' "$airgap_hash" '"}}' \
    >"$MANIFEST"
ssh-keygen -q -t ed25519 -N '' -f "$KEY"
ssh-keygen -Y sign -f "$KEY" -n hamn-k3s-compatibility "$MANIFEST" >/dev/null

run_installer() {
    HAMN_K3S_DEST="$DEST" \
    HAMN_K3S_AIRGAP_DIR="$AIRGAP_DIR" \
    HAMN_K3S_MANIFEST="$MANIFEST" \
    HAMN_K3S_MANIFEST_SIGNATURE="$MANIFEST.sig" \
    HAMN_K3S_PUBLIC_KEY="$KEY.pub" \
    HAMN_K3S_ALLOW_LOCAL_ARTIFACTS=1 \
    HAMN_K3S_ARCH=aarch64 \
        bash "$ROOT/scripts/install-k3s.sh"
}

run_installer
test -x "$DEST"
"$DEST" --version | grep -Fq "$VERSION"
test "$(sha256sum "$DEST" | awk '{print $1}')" = "$binary_hash"
test "$(sha256sum "$AIRGAP_DIR/k3s-airgap-images-arm64.tar.zst" | awk '{print $1}')" = "$airgap_hash"

# Existing data is checked against the signed digest; it is repaired rather
# than executed when a same-version but different binary appears.
printf '%s\n' '#!/bin/sh' "printf 'k3s version $VERSION\\n'" 'exit 9' >"$DEST"
chmod 0755 "$DEST"
run_installer
test "$(sha256sum "$DEST" | awk '{print $1}')" = "$binary_hash"

# A valid signature cannot authorize local URLs in production mode.
if HAMN_K3S_DEST="$DEST" HAMN_K3S_AIRGAP_DIR="$AIRGAP_DIR" \
    HAMN_K3S_MANIFEST="$MANIFEST" HAMN_K3S_MANIFEST_SIGNATURE="$MANIFEST.sig" \
    HAMN_K3S_PUBLIC_KEY="$KEY.pub" HAMN_K3S_ARCH=aarch64 \
    bash "$ROOT/scripts/install-k3s.sh" >"$WORK/local.out" 2>"$WORK/local.err"; then
    echo "FAIL: production K3s installer accepted local artifact URLs" >&2
    exit 1
fi
grep -Fq 'URL must use HTTPS' "$WORK/local.err"

printf '\n' >>"$MANIFEST"
if run_installer >"$WORK/tampered.out" 2>"$WORK/tampered.err"; then
    echo "FAIL: tampered K3s compatibility manifest was accepted" >&2
    exit 1
fi
grep -Fq 'signature verification failed' "$WORK/tampered.err"

echo "PASS: K3s binary and air-gap images require a signed compatibility manifest"
