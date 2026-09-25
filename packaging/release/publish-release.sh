#!/bin/bash
# Promote exact GitHub-hosted candidate bytes without rebuilding or using a
# long-lived release key. The workflow verifies GitHub attestations separately.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn publish: $*" >&2
    exit 1
}

safe_regular() {
    local path=$1
    [ -f "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(stat -f '%u:%l' "$path")" = "$(id -u):1" ]
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
STABLE_TAG=${1:-}
RC_TAG=${2:-}
RELEASE_REF=${3:-}
INPUT_DIR=${4:-}
OUTPUT_DIR=${5:-}
EXPECTED_WORKFLOW_RUN=${HAMN_EXPECTED_WORKFLOW_RUN:-}
EXPECTED_WORKFLOW_ATTEMPT=${HAMN_EXPECTED_WORKFLOW_ATTEMPT:-}
PROVENANCE=${HAMN_RELEASE_PROVENANCE:-workflow}
RELEASE_REPOSITORY=${HAMN_RELEASE_REPOSITORY:-}
RELEASE_BASE_URL=${HAMN_RELEASE_BASE_URL:-}
# Evidence verifier and manifest writer (tools/hamn-dev).
HAMN_DEV=${HAMN_DEV:-}

[ -n "$STABLE_TAG" ] && [ -n "$RC_TAG" ] && [ -n "$RELEASE_REF" ] &&
    [ -n "$INPUT_DIR" ] && [ -n "$OUTPUT_DIR" ] ||
    fail "usage: publish-release.sh vX.Y.Z vX.Y.Z-rc.N COMMIT INPUT_DIR OUTPUT_DIR"
[ -x "$HAMN_DEV" ] || fail "HAMN_DEV must name the built hamn-dev executable"
[[ "$STABLE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail "stable tag is invalid"
[[ "$RC_TAG" =~ ^${STABLE_TAG}-rc\.[0-9]+$ ]] ||
    fail "RC tag does not correspond to the stable tag"
case "$PROVENANCE" in
workflow)
    [[ "$EXPECTED_WORKFLOW_RUN" =~ ^[1-9][0-9]*$ ]] ||
        fail "HAMN_EXPECTED_WORKFLOW_RUN must be a positive decimal run ID"
    [[ "$EXPECTED_WORKFLOW_ATTEMPT" =~ ^[1-9][0-9]*$ ]] ||
        fail "HAMN_EXPECTED_WORKFLOW_ATTEMPT must be a positive decimal attempt"
    ;;
solo-local)
    [ "${GITHUB_ACTIONS:-}" != true ] ||
        fail "solo-local provenance is unavailable inside GitHub Actions"
    [ -z "$EXPECTED_WORKFLOW_RUN" ] && [ -z "$EXPECTED_WORKFLOW_ATTEMPT" ] ||
        fail "solo-local provenance must not accept workflow run inputs"
    EXPECTED_WORKFLOW_RUN=local
    EXPECTED_WORKFLOW_ATTEMPT=local
    ;;
*) fail "HAMN_RELEASE_PROVENANCE must be workflow or solo-local" ;;
esac
if [ -n "$RELEASE_REPOSITORY" ]; then
    [[ "$RELEASE_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
        fail "HAMN_RELEASE_REPOSITORY is invalid"
    [ -z "$RELEASE_BASE_URL" ] ||
        fail "HAMN_RELEASE_BASE_URL must not override the canonical GitHub Release base"
    BASE_URL="https://github.com/${RELEASE_REPOSITORY}/releases/download/${STABLE_TAG}"
else
    BASE_URL=$RELEASE_BASE_URL
    [ -n "$BASE_URL" ] || fail "HAMN_RELEASE_BASE_URL is required outside GitHub Actions"
fi
case "$BASE_URL" in https://*) ;; *) fail "release base URL must use HTTPS" ;; esac

[ -d "$INPUT_DIR" ] && [ ! -L "$INPUT_DIR" ] || fail "INPUT_DIR is unsafe"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] || fail "OUTPUT_DIR is unsafe"
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] ||
    fail "OUTPUT_DIR must be empty"

CANDIDATE_DIR=$INPUT_DIR/hamn-candidate
EVIDENCE_DIR=$INPUT_DIR/hamn-evidence
[ -d "$CANDIDATE_DIR" ] && [ ! -L "$CANDIDATE_DIR" ] &&
    [ -d "$EVIDENCE_DIR" ] && [ ! -L "$EVIDENCE_DIR" ] ||
    fail "candidate or hosted evidence directory is missing"
HOST_FILE="hamn-${STABLE_TAG}-darwin-arm64.tar.gz"
GUEST_FILE="hamn-${STABLE_TAG}-ubuntu-24.04-arm64.img"
SBOM_FILE="hamn-${STABLE_TAG}.spdx.json"
INSTALLER_FILE=install.sh
candidate=$CANDIDATE_DIR/candidate.json
checksums=$CANDIDATE_DIR/SHA256SUMS
evidence=$EVIDENCE_DIR/hosted-validation-evidence.json
for file in "$candidate" "$checksums" "$CANDIDATE_DIR/$HOST_FILE" \
    "$CANDIDATE_DIR/$GUEST_FILE" "$CANDIDATE_DIR/$SBOM_FILE" \
    "$CANDIDATE_DIR/$INSTALLER_FILE" "$evidence"; do
    safe_regular "$file" || fail "unsafe release input: $file"
done
(cd "$CANDIDATE_DIR" && shasum -a 256 -c SHA256SUMS) ||
    fail "candidate artifact hashes do not match"

COMMIT=$(git -C "$ROOT" rev-parse --verify "$RELEASE_REF^{commit}") ||
    fail "RELEASE_REF is not a commit"
SOURCE_TREE=$(git -C "$ROOT" rev-parse "$COMMIT^{tree}") ||
    fail "cannot resolve release source tree"
"$HAMN_DEV" release verify-hosted "$CANDIDATE_DIR" "$evidence" "$STABLE_TAG" \
    "$RC_TAG" "$COMMIT" "$SOURCE_TREE" "$EXPECTED_WORKFLOW_RUN" \
    "$EXPECTED_WORKFLOW_ATTEMPT" "$HOST_FILE" "$GUEST_FILE" "$SBOM_FILE" \
    "$INSTALLER_FILE"

SIZE_REPORT=$EVIDENCE_DIR/guest-image-size-report.json
SIZE_BUDGET=$ROOT/guest/image/release-size-budget.json
if [ -n "${HAMN_TEST_RELEASE_SIZE_BUDGET:-}" ]; then
    [ "${HAMN_RELEASE_ALLOW_LOCAL:-0}" = 1 ] && [ -z "${GITHUB_ACTIONS:-}" ] ||
        fail "test size budget is forbidden in release workflows"
    SIZE_BUDGET=$HAMN_TEST_RELEASE_SIZE_BUDGET
fi
make -s --no-print-directory -C "$ROOT/guest" image-tool >/dev/null &&
    "$ROOT/guest/build/hamn-image-tool" verify-release-size \
    "$CANDIDATE_DIR/$GUEST_FILE" "$SIZE_REPORT" "$SIZE_BUDGET" "$COMMIT" ||
    fail "guest image size evidence or reviewed release budget is missing or invalid"

# Only the schema v3 manifest is published; clients up to v0.1.2 read the
# removed v2 manifest and must reinstall with install.sh.
MANIFEST=$OUTPUT_DIR/hamn-update-manifest-v3.json
"$HAMN_DEV" release write-manifest "$MANIFEST" "$STABLE_TAG" "$COMMIT" \
    "$BASE_URL" "$CANDIDATE_DIR" "$HOST_FILE" "$GUEST_FILE"

# Validate the manifest with the exact candidate client's parser (schema,
# HTTPS URLs and compatibility) instead of a second implementation. The archive
# digest was verified above; extract only its executable, as install.sh does.
validator=$(mktemp -d "${TMPDIR:-/tmp}/hamn-publish-validator.XXXXXX") ||
    fail "cannot create manifest validator workspace"
trap 'rm -rf "$validator"' EXIT
validator_member=
while IFS= read -r member; do
    if [[ "$member" =~ ^[A-Za-z0-9._-]+/bin/hamn$ ]]; then
        [ -z "$validator_member" ] || fail "candidate host archive has duplicate executables"
        validator_member=$member
    fi
done < <(tar -tzf "$CANDIDATE_DIR/$HOST_FILE")
[ -n "$validator_member" ] || fail "candidate host archive has no executable"
tar -xzOf "$CANDIDATE_DIR/$HOST_FILE" -- "$validator_member" >"$validator/hamn" ||
    fail "cannot read the candidate executable"
chmod 0700 "$validator/hamn"
"$validator/hamn" __install-support upgrade fields "$MANIFEST" >/dev/null ||
    fail "the candidate client rejects the generated v3 manifest"
cp "$candidate" "$OUTPUT_DIR/candidate.json"
cp "$checksums" "$OUTPUT_DIR/SHA256SUMS"
cp "$evidence" "$OUTPUT_DIR/hosted-validation-evidence.json"
printf '%s\n' "$RC_TAG" >"$OUTPUT_DIR/promoted-from-rc"
chmod 0644 "$OUTPUT_DIR"/*
echo "verified hosted candidate ${RC_TAG}; publish exact bytes without rebuilding"
