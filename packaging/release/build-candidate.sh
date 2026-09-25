#!/bin/bash
# Build the exact bytes that a physical Apple Silicon validator will test.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn release candidate: $*" >&2
    exit 1
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

safe_regular() {
    local path=$1
    [ -f "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(stat -f '%u:%l' "$path")" = "$(id -u):1" ]
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
RELEASE_REF=${RELEASE_REF:-}
RELEASE_TAG=${RELEASE_TAG:-}
OUTPUT_DIR=${OUTPUT_DIR:-}
GUEST_IMAGE=${HAMN_GUEST_IMAGE:-}
ALLOW_DIRTY=${HAMN_RELEASE_ALLOW_DIRTY:-0}
ALLOW_LOCAL=${HAMN_RELEASE_ALLOW_LOCAL:-0}
RELEASE_REPOSITORY=${GITHUB_REPOSITORY:-${HAMN_RELEASE_REPOSITORY:-}}
# Release metadata writer (tools/hamn-dev); `make release-candidate` builds it.
HAMN_DEV=${HAMN_DEV:-}

[ -n "$RELEASE_REF" ] && [ -n "$RELEASE_TAG" ] && [ -n "$OUTPUT_DIR" ] ||
    fail "RELEASE_REF, RELEASE_TAG, and OUTPUT_DIR are required"
[ -x "$HAMN_DEV" ] || fail "HAMN_DEV must name the built hamn-dev executable"
if [[ ! "$RELEASE_TAG" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)-rc\.([0-9]+)$ ]]; then
    fail "RELEASE_TAG must be a vX.Y.Z-rc.N tag"
fi
VERSION=v${BASH_REMATCH[1]}
[ "$(uname -m)" = arm64 ] ||
    fail "release candidate must build on Apple Silicon arm64"
COMMIT=$(git -C "$ROOT" rev-parse --verify "$RELEASE_REF^{commit}") ||
    fail "RELEASE_REF is not a commit"
[ "$COMMIT" = "$(git -C "$ROOT" rev-parse HEAD)" ] ||
    fail "RELEASE_REF does not match the checked-out commit"
SOURCE_TREE=$(git -C "$ROOT" rev-parse "$COMMIT^{tree}") ||
    fail "cannot resolve the checked-out source tree"
COMMIT_EPOCH=$(git -C "$ROOT" show -s --format=%ct "$COMMIT") ||
    fail "cannot resolve the checked-out commit timestamp"
[[ "$COMMIT_EPOCH" =~ ^[0-9]+$ ]] ||
    fail "checked-out commit timestamp is invalid"
if [ "$ALLOW_DIRTY" != 1 ] && [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
    fail "release source tree is dirty"
fi

safe_regular "$GUEST_IMAGE" ||
    fail "HAMN_GUEST_IMAGE must name one owned regular guest image"

MANIFEST_URL=${HAMN_RELEASE_MANIFEST_URL:-}
if [ -n "$RELEASE_REPOSITORY" ]; then
    [[ "$RELEASE_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
        fail "GITHUB_REPOSITORY is invalid"
    CANONICAL_MANIFEST_URL="https://github.com/${RELEASE_REPOSITORY}/releases/latest/download/hamn-update-manifest-v3.json"
    if [ -n "$MANIFEST_URL" ] && [ "$MANIFEST_URL" != "$CANONICAL_MANIFEST_URL" ]; then
        fail "HAMN_RELEASE_MANIFEST_URL must match the canonical GitHub Release manifest URL"
    fi
    MANIFEST_URL=$CANONICAL_MANIFEST_URL
fi
[ -n "$MANIFEST_URL" ] ||
    fail "HAMN_RELEASE_MANIFEST_URL is required outside GitHub Actions"
case "$MANIFEST_URL" in
https://*) ;;
file://*|/*)
    [ "$ALLOW_LOCAL" = 1 ] || fail "release manifest URL must use HTTPS"
    ;;
*) fail "release manifest URL must use HTTPS" ;;
esac

mkdir -p "$OUTPUT_DIR"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] ||
    fail "OUTPUT_DIR is unsafe"
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] ||
    fail "OUTPUT_DIR must be empty"
WORK=$(mktemp -d "$OUTPUT_DIR/.hamn-candidate.XXXXXX") ||
    fail "cannot create candidate workspace"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

make -C "$ROOT" host VERSION="${VERSION#v}" >/dev/null
"$ROOT/build/hamn" --version | grep -Fxq "hamn ${VERSION#v}" ||
    fail "candidate binary version does not match tag"

ARTIFACT_ROOT="$WORK/hamn-${VERSION}-darwin-arm64"
mkdir -m 0755 "$ARTIFACT_ROOT"
mkdir -m 0755 "$ARTIFACT_ROOT/bin"
install -m 0755 "$ROOT/build/hamn" "$ARTIFACT_ROOT/bin/hamn"
# Stage through a file: a reading tar stops at the end-of-archive marker, so
# a writer still sending the final record's padding can fail with EPIPE
# ("tar: Write error", CI run 36145944094).
git -C "$ROOT" ls-files -z -- scripts packaging |
    tar -C "$ROOT" --null -T - -cf "$WORK/sources.tar"
tar -C "$ARTIFACT_ROOT" -xf "$WORK/sources.tar"
rm "$WORK/sources.tar"
printf '%s\n' "$MANIFEST_URL" \
    >"$ARTIFACT_ROOT/packaging/release/update-manifest-url"
chmod 0644 "$ARTIFACT_ROOT/packaging/release/update-manifest-url"

HOST_ARTIFACT="$OUTPUT_DIR/hamn-${VERSION}-darwin-arm64.tar.gz"
COPYFILE_DISABLE=1 tar -C "$WORK" -czf "$HOST_ARTIFACT" \
    "$(basename "$ARTIFACT_ROOT")"
GUEST_ARTIFACT="$OUTPUT_DIR/hamn-${VERSION}-ubuntu-24.04-arm64.img"
cp "$GUEST_IMAGE" "$GUEST_ARTIFACT"
chmod 0644 "$GUEST_ARTIFACT"
INSTALLER="$OUTPUT_DIR/install.sh"

HOST_HASH=$(sha256_file "$HOST_ARTIFACT")
GUEST_HASH=$(sha256_file "$GUEST_ARTIFACT")
if [ -n "$RELEASE_REPOSITORY" ]; then
    RELEASE_BASE="https://github.com/${RELEASE_REPOSITORY}/releases/download/${VERSION}"
    HOST_URL="$RELEASE_BASE/$(basename "$HOST_ARTIFACT")"
    GUEST_URL="$RELEASE_BASE/$(basename "$GUEST_ARTIFACT")"
else
    [ "$ALLOW_LOCAL" = 1 ] || fail "HAMN_RELEASE_REPOSITORY is required"
    HOST_URL="file://$HOST_ARTIFACT"
    GUEST_URL="file://$GUEST_ARTIFACT"
fi
"$HAMN_DEV" release render-installer "$ROOT/packaging/release/install.sh.in" \
    "$INSTALLER" "$VERSION" "$COMMIT" "$HOST_URL" "$HOST_HASH" "$GUEST_URL" \
    "$GUEST_HASH" "$HOST_ARTIFACT" "$GUEST_ARTIFACT"
chmod 0755 "$INSTALLER"
INSTALLER_HASH=$(sha256_file "$INSTALLER")
SBOM="$OUTPUT_DIR/hamn-${VERSION}.spdx.json"
"$HAMN_DEV" release write-sbom "$SBOM" "$VERSION" "$COMMIT" "$SOURCE_TREE" \
    "$COMMIT_EPOCH" "$(basename "$HOST_ARTIFACT")" "$HOST_HASH" \
    "$(basename "$GUEST_ARTIFACT")" "$GUEST_HASH"
chmod 0644 "$SBOM"
SBOM_HASH=$(sha256_file "$SBOM")

CANDIDATE="$OUTPUT_DIR/candidate.json"
"$HAMN_DEV" release write-candidate "$CANDIDATE" "$RELEASE_TAG" "$VERSION" \
    "$COMMIT" "$SOURCE_TREE" \
    "$(basename "$HOST_ARTIFACT")" "$HOST_HASH" \
    "$(basename "$GUEST_ARTIFACT")" "$GUEST_HASH" \
    "$(basename "$INSTALLER")" "$INSTALLER_HASH" \
    "$(basename "$SBOM")" "$SBOM_HASH"
chmod 0644 "$CANDIDATE"

{
    printf '%s  %s\n' "$HOST_HASH" "$(basename "$HOST_ARTIFACT")"
    printf '%s  %s\n' "$GUEST_HASH" "$(basename "$GUEST_ARTIFACT")"
    printf '%s  %s\n' "$INSTALLER_HASH" "$(basename "$INSTALLER")"
    printf '%s  %s\n' "$SBOM_HASH" "$(basename "$SBOM")"
    printf '%s  %s\n' "$(sha256_file "$CANDIDATE")" "$(basename "$CANDIDATE")"
} >"$OUTPUT_DIR/SHA256SUMS"
chmod 0644 "$OUTPUT_DIR/SHA256SUMS"

echo "built candidate ${RELEASE_TAG} (${COMMIT}) in ${OUTPUT_DIR}"
