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
BASELINE_OUTPUT=${HAMN_GUEST_BASELINE_OUTPUT:-}
VIRT_CUSTOMIZE=${HAMN_VIRT_CUSTOMIZE:-virt-customize}
QEMU_IMG=${HAMN_QEMU_IMG:-qemu-img}
VIRT_RESIZE=${HAMN_VIRT_RESIZE:-virt-resize}
GUESTFISH=${HAMN_GUESTFISH:-guestfish}
TARGET_SIZE=8G
REVIEW_ONLY=${HAMN_GUEST_SIZE_REVIEW_ONLY:-0}
[[ "$REVIEW_ONLY" = 0 || "$REVIEW_ONLY" = 1 ]] || fail "invalid size review mode"
MAX_RELEASE_ASSET_SIZE=2147483648

[ -n "$BASE_IMAGE" ] && [ -n "$BASE_SHA256" ] && [ -n "$OUTPUT" ] ||
    fail "HAMN_GUEST_BASE_IMAGE, HAMN_GUEST_BASE_SHA256, and HAMN_GUEST_OUTPUT are required"
[[ "$BASE_SHA256" =~ ^[0-9a-f]{64}$ ]] ||
    fail "HAMN_GUEST_BASE_SHA256 must be lowercase SHA-256"
safe_regular "$BASE_IMAGE" || fail "base image is unsafe"
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
for artifact in "$OUTPUT" "$OUTPUT.sha256" "$OUTPUT.packages-before.tsv" \
    "$OUTPUT.packages-after.tsv" "$OUTPUT.size-report.json" "$OUTPUT.size-report.budget-proposal.json"; do
    [ ! -e "$artifact" ] && [ ! -L "$artifact" ] ||
        fail "guest image output already exists: $artifact"
done
if [ -n "$BASELINE_OUTPUT" ]; then
    python3 "$ROOT/guest/image/image_evidence.py" check "$BASELINE_OUTPUT" \
        "$OUTPUT" "$OUTPUT.sha256" "$OUTPUT.packages-before.tsv" \
        "$OUTPUT.packages-after.tsv" "$OUTPUT.size-report.json" \
        "$OUTPUT.size-report.budget-proposal.json"
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/hamn-guest-image.XXXXXX") ||
    fail "cannot create image build workspace"
STAGE_DIR=$(mktemp -d "$OUTPUT_DIR/.hamn-guest-image.XXXXXX") ||
    fail "cannot create private image stage"
STAGE=$STAGE_DIR/image.img
COMPACT=$STAGE_DIR/compressed.img
cleanup() {
    rm -rf "$WORK"
    rm -f "$STAGE" "$COMPACT"
    rmdir "$STAGE_DIR"
}
trap cleanup EXIT

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
BUILD_PACKAGES='gcc,make'
PACKAGES='python3,curl,docker.io,containerd,runc,containernetworking-plugins,qemu-user-static,binfmt-support,dnsmasq,nftables'
PROVISION=$WORK/provision.sh
cat >"$PROVISION" <<'EOF'
#!/bin/bash
set -euo pipefail
install -d -m 0755 /etc/hamn /opt/hamn
install -m 0644 /tmp/hamn-guest-image.json /etc/hamn/guest-image.json
tar -xzf /tmp/hamn-guest-sources.tar.gz -C /opt/hamn
getent group hamn >/dev/null || groupadd --system hamn
make -C /opt/hamn/guest install
systemctl enable hamnd.service
rm -rf /opt/hamn/guest /opt/hamn/vendor \
    /tmp/hamn-guest-sources.tar.gz
QEMU_BINFMT_INTERPRETER=/usr/bin/qemu-x86_64-static
test -x "$QEMU_BINFMT_INTERPRETER" && test ! -L "$QEMU_BINFMT_INTERPRETER"
printf '%s\n' \
    'package qemu-user-static' \
    "interpreter $QEMU_BINFMT_INTERPRETER" \
    'magic \x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00' \
    'mask \xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff' \
    'credentials no' \
    'preserve yes' \
    'fix_binary yes' \
    > /usr/share/binfmts/qemu-x86_64
chmod 0644 /usr/share/binfmts/qemu-x86_64
update-binfmts --import qemu-x86_64
systemctl enable binfmt-support.service
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
    --run-command 'timeout 30 getent ahostsv4 ports.ubuntu.com' \
    --install "$BUILD_PACKAGES,$PACKAGES" \
    --upload "$GUEST_MANIFEST:/tmp/hamn-guest-image.json" \
    --upload "$SOURCE_ARCHIVE:/tmp/hamn-guest-sources.tar.gz" \
    --upload "$PROVISION:/tmp/hamn-image-provision.sh" \
    --run-command 'bash /tmp/hamn-image-provision.sh' \
    --run-command 'rm -f /tmp/hamn-image-provision.sh'

# Capture a compressed baseline before cleanup from this exact provisioned
# filesystem; separate builds could resolve different apt package versions.
BASELINE=$WORK/baseline.img
"$QEMU_IMG" convert -q -f qcow2 -O qcow2 \
    -o compression_type=zlib -c "$STAGE" "$BASELINE"
if [ -n "$BASELINE_OUTPUT" ]; then
    "$QEMU_IMG" compare -q -f qcow2 -F qcow2 "$STAGE" "$BASELINE" ||
        fail "baseline export changed provisioned guest-visible bytes"
fi
"$GUESTFISH" --ro --format=qcow2 -a "$STAGE" -i \
    command 'dpkg-query -W -f=${Package}\t${Version}\t${Installed-Size}\n' \
    >"$OUTPUT.packages-before.tsv"
"$VIRT_CUSTOMIZE" -a "$STAGE" \
    --upload "$ROOT/guest/image/slim-guest.sh:/root/hamn-image-slim.sh" \
    --run-command 'bash /root/hamn-image-slim.sh && rm /root/hamn-image-slim.sh'
"$GUESTFISH" --ro --format=qcow2 -a "$STAGE" -i \
    command 'dpkg-query -W -f=${Package}\t${Version}\t${Installed-Size}\n' \
    >"$OUTPUT.packages-after.tsv"
# fstrim is an offline libguestfs filesystem operation; unavailable discard is
# a build failure, not an excuse to claim a sparse image without evidence.
"$GUESTFISH" --rw add-drive "$STAGE" format:qcow2 discard:enable \
    : run : mount /dev/sda3 / : fstrim / : umount-all : shutdown

"$QEMU_IMG" convert -q -f qcow2 -O qcow2 \
    -o compression_type=zlib -c "$STAGE" "$COMPACT"
"$QEMU_IMG" compare -q -f qcow2 -F qcow2 "$STAGE" "$COMPACT" ||
    fail "compressed guest image changed guest-visible bytes"
OUTPUT_SIZE=$(wc -c <"$COMPACT" | tr -d '[:space:]')
[[ "$OUTPUT_SIZE" =~ ^[0-9]+$ ]] &&
    [ "$OUTPUT_SIZE" -lt "$MAX_RELEASE_ASSET_SIZE" ] ||
    fail "compressed guest image exceeds the GitHub release asset limit"
# Validate the decoder shipped to hosts independently against qemu-img.
cc -D_GNU_SOURCE -std=c11 -O2 -Wall -Wextra -Werror=implicit-function-declaration \
    -I"$ROOT/host" "$ROOT/guest/image/extract-check.c" \
    "$ROOT/host/image/qcow2.c" -lz -o "$WORK/extract-check"
"$WORK/extract-check" "$COMPACT" "$WORK/extracted.raw"
"$QEMU_IMG" convert -q -f qcow2 -O raw "$COMPACT" "$WORK/reference.raw"
[ "$(sha256_file "$WORK/extracted.raw")" = "$(sha256_file "$WORK/reference.raw")" ] || \
    fail "custom extractor differs from qemu-img reference bytes"
python3 "$ROOT/guest/image/verify-raw.py" "$WORK/extracted.raw"
SIZE_OPTIONS=()
[ "$REVIEW_ONLY" = 0 ] || SIZE_OPTIONS+=(--review-only)
python3 "$ROOT/guest/image/verify-size.py" \
    --baseline "$BASELINE" --candidate "$COMPACT" \
    --packages-before "$OUTPUT.packages-before.tsv" --packages-after "$OUTPUT.packages-after.tsv" \
    --report "$OUTPUT.size-report.json" --budget "$ROOT/guest/image/release-size-budget.json" \
    --base-sha256 "$BASE_SHA256" --source-revision "$(git -C "$ROOT" rev-parse HEAD)" \
    "${SIZE_OPTIONS[@]}"
rm -f "$STAGE"
STAGE=
mv -f "$COMPACT" "$OUTPUT"
COMPACT=
printf '%s  %s\n' "$(sha256_file "$OUTPUT")" "$(basename "$OUTPUT")" \
    >"$OUTPUT.sha256"
chmod 0644 "$OUTPUT" "$OUTPUT.sha256"
if [ -n "$BASELINE_OUTPUT" ]; then
    # Preserve the actual pre-cleanup artifact only after every existing gate.
    python3 "$ROOT/guest/image/image_evidence.py" publish \
        "$BASELINE" "$BASELINE_OUTPUT" "$OUTPUT.size-report.json"
fi
if [ "$REVIEW_ONLY" = 1 ]; then
    echo "built review-only guest image; review size and physical runtime evidence before distribution: $OUTPUT"
else
    echo "built preconfigured Hamn Ubuntu 24.04 arm64 guest image: $OUTPUT"
fi
