#!/bin/bash
# Build the immutable Ubuntu 24.04 arm64 VM image used by signed Hamn releases.
# This runs on a trusted Linux arm64 image-builder with libguestfs installed;
# it is deliberately not part of the hosted macOS candidate build.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn guest image: $*" >&2
    exit 1
}

sha256_file() {
    sha256sum "$1" | awk '{print $1}'
}

safe_regular() {
    [ -f "$1" ] && [ ! -L "$1" ]
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
BASE_IMAGE=${HAMN_GUEST_BASE_IMAGE:-}
BASE_SHA256=${HAMN_GUEST_BASE_SHA256:-}
OUTPUT=${HAMN_GUEST_OUTPUT:-}
K3S_MANIFEST=${HAMN_K3S_COMPATIBILITY_MANIFEST:-}
K3S_SIGNATURE=${HAMN_K3S_COMPATIBILITY_SIGNATURE:-}
RELEASE_PUBLIC_KEY=${HAMN_RELEASE_PUBLIC_KEY:-}
VIRT_CUSTOMIZE=${HAMN_VIRT_CUSTOMIZE:-virt-customize}
QEMU_IMG=${HAMN_QEMU_IMG:-qemu-img}
VIRT_RESIZE=${HAMN_VIRT_RESIZE:-virt-resize}
GUESTFISH=${HAMN_GUESTFISH:-guestfish}
TARGET_SIZE=8G

[ -n "$BASE_IMAGE" ] && [ -n "$BASE_SHA256" ] && [ -n "$OUTPUT" ] &&
    [ -n "$K3S_MANIFEST" ] && [ -n "$K3S_SIGNATURE" ] &&
    [ -n "$RELEASE_PUBLIC_KEY" ] ||
    fail "HAMN_GUEST_BASE_IMAGE, HAMN_GUEST_BASE_SHA256, HAMN_GUEST_OUTPUT, HAMN_K3S_COMPATIBILITY_MANIFEST, HAMN_K3S_COMPATIBILITY_SIGNATURE, and HAMN_RELEASE_PUBLIC_KEY are required"
[[ "$BASE_SHA256" =~ ^[0-9a-f]{64}$ ]] ||
    fail "HAMN_GUEST_BASE_SHA256 must be lowercase SHA-256"
safe_regular "$BASE_IMAGE" || fail "base image is unsafe"
safe_regular "$K3S_MANIFEST" || fail "K3s compatibility manifest is unsafe"
safe_regular "$K3S_SIGNATURE" || fail "K3s compatibility signature is unsafe"
safe_regular "$RELEASE_PUBLIC_KEY" || fail "release public key is unsafe"
ssh-keygen -lf "$RELEASE_PUBLIC_KEY" | grep -q ED25519 ||
    fail "release public key is not Ed25519"
[ "$(sha256_file "$BASE_IMAGE")" = "$BASE_SHA256" ] ||
    fail "base image SHA-256 mismatch"
command -v "$VIRT_CUSTOMIZE" >/dev/null 2>&1 ||
    fail "virt-customize (libguestfs) is required on the trusted image builder"
command -v "$QEMU_IMG" >/dev/null 2>&1 ||
    fail "qemu-img is required on the trusted image builder"
command -v "$VIRT_RESIZE" >/dev/null 2>&1 ||
    fail "virt-resize (libguestfs) is required on the trusted image builder"
command -v "$GUESTFISH" >/dev/null 2>&1 ||
    fail "guestfish (libguestfs) is required on the trusted image builder"

OUTPUT_DIR=$(dirname "$OUTPUT")
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] ||
    fail "guest image output directory is unsafe"
[ ! -e "$OUTPUT" ] && [ ! -L "$OUTPUT" ] ||
    fail "guest image output already exists"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/hamn-guest-image.XXXXXX") ||
    fail "cannot create image build workspace"
STAGE=$OUTPUT_DIR/.hamn-guest-image.$$.img
cleanup() {
    rm -rf "$WORK"
    rm -f "$STAGE"
}
trap cleanup EXIT

allowed=$WORK/allowed-signers
printf 'hamn-release ' >"$allowed"
cat "$RELEASE_PUBLIC_KEY" >>"$allowed"
ssh-keygen -Y verify -f "$allowed" -I hamn-release \
    -n hamn-k3s-compatibility -s "$K3S_SIGNATURE" <"$K3S_MANIFEST" >/dev/null ||
    fail "K3s compatibility manifest signature verification failed"

GUEST_MANIFEST=$WORK/guest-image.json
printf '%s\n' \
    '{"schemaVersion":1,"distribution":"ubuntu-24.04","architecture":"arm64",' \
    '"components":["docker","buildkit","containerd","runc","cni","binfmt","dnsmasq","hamnd"]}' \
    >"$GUEST_MANIFEST"
SOURCE_ARCHIVE=$WORK/hamn-guest-sources.tar.gz
git -C "$ROOT" rev-parse --is-inside-work-tree | grep -qx true ||
    fail "guest image builder must run from a Git checkout"
COMMIT_EPOCH=$(git -C "$ROOT" show -s --format=%ct HEAD) ||
    fail "cannot resolve guest image source timestamp"
[[ "$COMMIT_EPOCH" =~ ^[1-9][0-9]*$ ]] ||
    fail "guest image source timestamp is invalid"
git -C "$ROOT" archive --format=tar HEAD -- guest vendor |
    gzip -n >"$SOURCE_ARCHIVE" ||
    fail "cannot archive tracked guest image sources"

# The Docker Engine package is Ubuntu's Moby-derived docker.io package. Its
# default Buildx driver uses BuildKit server components embedded in dockerd.
# Buildx remains a host Docker CLI plugin; its docker-container driver runs a
# dedicated BuildKit container in this guest through the Docker API.
PACKAGES='gcc,make,python3,curl,docker.io,containerd,runc,containernetworking-plugins,qemu-user-static,binfmt-support,dnsmasq,nftables'
PROVISION=$WORK/provision.sh
cat >"$PROVISION" <<'EOF'
#!/bin/bash
set -euo pipefail
install -d -m 0755 /etc/hamn /opt/hamn
install -m 0644 /tmp/hamn-guest-image.json /etc/hamn/guest-image.json
install -m 0644 /tmp/k3s-compatibility.json /etc/hamn/k3s-compatibility.json
install -m 0644 /tmp/k3s-compatibility.json.sig /etc/hamn/k3s-compatibility.json.sig
install -m 0644 /tmp/hamn-release.pub /etc/hamn/hamn-release.pub
tar -xzf /tmp/hamn-guest-sources.tar.gz -C /opt/hamn
getent group hamn >/dev/null || groupadd --system hamn
make -C /opt/hamn/guest install
systemctl enable hamnd.service
rm -rf /opt/hamn/guest /opt/hamn/vendor \
    /tmp/hamn-guest-sources.tar.gz
QEMU_BINFMT_SOURCE=/usr/share/doc/qemu-user-static/qemu-x86_64.conf
test -f "$QEMU_BINFMT_SOURCE" && test ! -L "$QEMU_BINFMT_SOURCE"
test -f /usr/lib/binfmt.d/qemu-x86_64.conf && \
    test ! -L /usr/lib/binfmt.d/qemu-x86_64.conf
install -m 0644 "$QEMU_BINFMT_SOURCE" /usr/share/binfmts/qemu-x86_64
update-binfmts --import qemu-x86_64
update-binfmts --enable qemu-x86_64
EOF
chmod 0755 "$PROVISION"

"$QEMU_IMG" create -q -f qcow2 "$STAGE" "$TARGET_SIZE"
"$VIRT_RESIZE" --format qcow2 --output-format qcow2 \
    --no-expand-content --expand /dev/sda1 "$BASE_IMAGE" "$STAGE"
if ! ROOT_LABEL=$("$GUESTFISH" --ro --format=qcow2 -a "$STAGE" <<'GUESTFISH_LABEL_COMMANDS'
run
vfs-label /dev/sda3
GUESTFISH_LABEL_COMMANDS
); then
    fail "cannot verify the resized guest root filesystem"
fi
[ "$ROOT_LABEL" = cloudimg-rootfs ] ||
    fail "resized guest root filesystem label is invalid"
if ! "$GUESTFISH" --rw --format=qcow2 -a "$STAGE" <<'GUESTFISH_RESIZE_COMMANDS'
run
e2fsck-f /dev/sda3
debug sh "resize2fs -f /dev/sda3"
GUESTFISH_RESIZE_COMMANDS
then
    fail "cannot check and force-expand the resized guest root filesystem"
fi
"$VIRT_CUSTOMIZE" -a "$STAGE" \
    --run-command "date -u -s '@$COMMIT_EPOCH'" \
    --install "$PACKAGES" \
    --upload "$GUEST_MANIFEST:/tmp/hamn-guest-image.json" \
    --upload "$K3S_MANIFEST:/tmp/k3s-compatibility.json" \
    --upload "$K3S_SIGNATURE:/tmp/k3s-compatibility.json.sig" \
    --upload "$RELEASE_PUBLIC_KEY:/tmp/hamn-release.pub" \
    --upload "$SOURCE_ARCHIVE:/tmp/hamn-guest-sources.tar.gz" \
    --upload "$PROVISION:/tmp/hamn-image-provision.sh" \
    --run-command 'bash /tmp/hamn-image-provision.sh' \
    --run-command 'rm -f /tmp/hamn-image-provision.sh'

mv -f "$STAGE" "$OUTPUT"
STAGE=
printf '%s  %s\n' "$(sha256_file "$OUTPUT")" "$(basename "$OUTPUT")" \
    >"$OUTPUT.sha256"
chmod 0644 "$OUTPUT" "$OUTPUT.sha256"
echo "built preconfigured Hamn Ubuntu 24.04 arm64 guest image: $OUTPUT"
