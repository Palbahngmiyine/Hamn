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
PUBLIC_KEY=${HAMN_RELEASE_PUBLIC_KEY:-}
ALLOW_DIRTY=${HAMN_RELEASE_ALLOW_DIRTY:-0}
ALLOW_LOCAL=${HAMN_RELEASE_ALLOW_LOCAL:-0}

[ -n "$RELEASE_REF" ] && [ -n "$RELEASE_TAG" ] && [ -n "$OUTPUT_DIR" ] ||
    fail "RELEASE_REF, RELEASE_TAG, and OUTPUT_DIR are required"
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
safe_regular "$PUBLIC_KEY" ||
    fail "HAMN_RELEASE_PUBLIC_KEY must name one owned regular public key"
ssh-keygen -lf "$PUBLIC_KEY" | grep -q 'ED25519' ||
    fail "HAMN_RELEASE_PUBLIC_KEY is not Ed25519"

MANIFEST_URL=${HAMN_RELEASE_MANIFEST_URL:-}
if [ -n "${GITHUB_REPOSITORY:-}" ]; then
    [[ "$GITHUB_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
        fail "GITHUB_REPOSITORY is invalid"
    CANONICAL_MANIFEST_URL="https://github.com/${GITHUB_REPOSITORY}/releases/download/${VERSION}/hamn-update-manifest.json"
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
"$ROOT/build/hamn" version | grep -Fxq "hamn ${VERSION#v}" ||
    fail "candidate binary version does not match tag"

ARTIFACT_ROOT="$WORK/hamn-${VERSION}-darwin-arm64"
mkdir -m 0755 "$ARTIFACT_ROOT"
mkdir -m 0755 "$ARTIFACT_ROOT/bin"
install -m 0755 "$ROOT/build/hamn" "$ARTIFACT_ROOT/bin/hamn"
rsync -a --delete --exclude build --exclude '._*' \
    "$ROOT/scripts" "$ROOT/packaging" \
    "$ARTIFACT_ROOT/"
install -m 0644 "$PUBLIC_KEY" \
    "$ARTIFACT_ROOT/packaging/release/hamn-release.pub"
printf '%s\n' "$MANIFEST_URL" \
    >"$ARTIFACT_ROOT/packaging/release/update-manifest-url"
chmod 0644 "$ARTIFACT_ROOT/packaging/release/update-manifest-url"

python3 - "$ROOT/packaging/release/install.sh.in" \
    "$ARTIFACT_ROOT/packaging/release/install.sh" "$PUBLIC_KEY" \
    "$MANIFEST_URL" <<'PY'
import base64
import json
import sys

template_path, output_path, key_path, manifest_url = sys.argv[1:]
with open(template_path, encoding="utf-8") as source:
    template = source.read()
with open(key_path, "rb") as source:
    key = source.read()
if not key or not key.endswith(b"\n"):
    raise SystemExit("release public key is malformed")
if template.count("__HAMN_RELEASE_PUBLIC_KEY_B64__") != 1 or \
        template.count("__HAMN_RELEASE_MANIFEST_URL__") != 1:
    raise SystemExit("installer template placeholders are malformed")
rendered = template.replace("__HAMN_RELEASE_PUBLIC_KEY_B64__",
                            base64.b64encode(key).decode("ascii"))
rendered = rendered.replace("__HAMN_RELEASE_MANIFEST_URL__",
                            json.dumps(manifest_url))
if "__HAMN_" in rendered:
    raise SystemExit("installer template has an unresolved placeholder")
with open(output_path, "w", encoding="utf-8", newline="\n") as output:
    output.write(rendered)
PY
chmod 0755 "$ARTIFACT_ROOT/packaging/release/install.sh"

HOST_ARTIFACT="$OUTPUT_DIR/hamn-${VERSION}-darwin-arm64.tar.gz"
COPYFILE_DISABLE=1 tar -C "$WORK" -czf "$HOST_ARTIFACT" \
    "$(basename "$ARTIFACT_ROOT")"
GUEST_ARTIFACT="$OUTPUT_DIR/hamn-${VERSION}-ubuntu-24.04-arm64.img"
cp "$GUEST_IMAGE" "$GUEST_ARTIFACT"
chmod 0644 "$GUEST_ARTIFACT"
INSTALLER="$OUTPUT_DIR/install.sh"
install -m 0755 "$ARTIFACT_ROOT/packaging/release/install.sh" "$INSTALLER"

HOST_HASH=$(sha256_file "$HOST_ARTIFACT")
GUEST_HASH=$(sha256_file "$GUEST_ARTIFACT")
INSTALLER_HASH=$(sha256_file "$INSTALLER")
SBOM="$OUTPUT_DIR/hamn-${VERSION}.spdx.json"
python3 - "$SBOM" "$VERSION" "$COMMIT" "$SOURCE_TREE" "$COMMIT_EPOCH" \
    "$(basename "$HOST_ARTIFACT")" "$HOST_HASH" \
    "$(basename "$GUEST_ARTIFACT")" "$GUEST_HASH" <<'PY'
import datetime
import json
import sys

(path, version, commit, tree, commit_epoch, host_name, host_hash, guest_name,
 guest_hash) = sys.argv[1:]
created = datetime.datetime.fromtimestamp(int(commit_epoch),
                                          datetime.timezone.utc).isoformat()
created = created.replace("+00:00", "Z")
document = {
    "SPDXID": "SPDXRef-DOCUMENT",
    "spdxVersion": "SPDX-2.3",
    "name": "Hamn " + version,
    "dataLicense": "CC0-1.0",
    "documentNamespace": "https://hamn.dev/spdx/" + version + "/" + commit,
    "creationInfo": {
        "creators": ["Tool: hamn-release-candidate"],
        "created": created,
        "licenseListVersion": "3.23",
    },
    "packages": [
        {
            "SPDXID": "SPDXRef-HamnHost",
            "name": host_name,
            "versionInfo": version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "checksums": [{"algorithm": "SHA256", "checksumValue": host_hash}],
        },
        {
            "SPDXID": "SPDXRef-HamnGuest",
            "name": guest_name,
            "versionInfo": version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "checksums": [{"algorithm": "SHA256", "checksumValue": guest_hash}],
        },
    ],
    "annotations": [{
        "annotationType": "OTHER",
        "annotator": "Tool: hamn-release-candidate",
        "comment": "commit=" + commit + " sourceTree=" + tree,
    }],
}
with open(path, "w", encoding="utf-8", newline="\n") as output:
    json.dump(document, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
chmod 0644 "$SBOM"
SBOM_HASH=$(sha256_file "$SBOM")

CANDIDATE="$OUTPUT_DIR/candidate.json"
python3 - "$CANDIDATE" "$RELEASE_TAG" "$VERSION" "$COMMIT" "$SOURCE_TREE" \
    "$(basename "$HOST_ARTIFACT")" "$HOST_HASH" \
    "$(basename "$GUEST_ARTIFACT")" "$GUEST_HASH" \
    "$(basename "$INSTALLER")" "$INSTALLER_HASH" \
    "$(basename "$SBOM")" "$SBOM_HASH" <<'PY'
import json
import sys

(path, tag, version, commit, tree, host_name, host_hash, guest_name,
 guest_hash, installer_name, installer_hash, sbom_name, sbom_hash) = sys.argv[1:]
document = {
    "schemaVersion": 1,
    "kind": "hamn-release-candidate",
    "tag": tag,
    "version": version,
    "commit": commit,
    "sourceTree": tree,
    "artifacts": [
        {"name": host_name, "sha256": host_hash},
        {"name": guest_name, "sha256": guest_hash},
        {"name": installer_name, "sha256": installer_hash},
        {"name": sbom_name, "sha256": sbom_hash},
    ],
}
with open(path, "w", encoding="utf-8", newline="\n") as output:
    json.dump(document, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
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
