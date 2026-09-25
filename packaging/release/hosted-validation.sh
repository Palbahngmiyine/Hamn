#!/bin/bash
# Bind a GitHub-hosted regression result to exact candidate bytes. This does
# not claim that Virtualization.framework or a live Hamn VM was exercised.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn hosted validation: $*" >&2
    exit 1
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
RELEASE_REF=${RELEASE_REF:-}
RELEASE_TAG=${RELEASE_TAG:-}
CANDIDATE_DIR=${CANDIDATE_DIR:-}
OUTPUT_DIR=${OUTPUT_DIR:-}
RUN_ID=${GITHUB_RUN_ID:-local}
RUN_ATTEMPT=${GITHUB_RUN_ATTEMPT:-local}
# Evidence writer (tools/hamn-dev); `make release-hosted-validation` builds it.
HAMN_DEV=${HAMN_DEV:-}

[ -n "$RELEASE_REF" ] && [ -n "$RELEASE_TAG" ] &&
    [ -n "$CANDIDATE_DIR" ] && [ -n "$OUTPUT_DIR" ] ||
    fail "RELEASE_REF, RELEASE_TAG, CANDIDATE_DIR, and OUTPUT_DIR are required"
[ -x "$HAMN_DEV" ] || fail "HAMN_DEV must name the built hamn-dev executable"
[[ "$RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+$ ]] ||
    fail "RELEASE_TAG must be a release candidate tag"
case "$RUN_ID:$RUN_ATTEMPT" in
local:local) ;;
*)
    [[ "$RUN_ID" =~ ^[1-9][0-9]*$ ]] &&
        [[ "$RUN_ATTEMPT" =~ ^[1-9][0-9]*$ ]] ||
        fail "workflow run and attempt must be positive decimals"
    ;;
esac
[ -d "$CANDIDATE_DIR" ] && [ ! -L "$CANDIDATE_DIR" ] ||
    fail "CANDIDATE_DIR is unsafe"
mkdir -p "$OUTPUT_DIR"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] ||
    fail "OUTPUT_DIR is unsafe"
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] ||
    fail "OUTPUT_DIR must be empty"

COMMIT=$(git -C "$ROOT" rev-parse --verify "$RELEASE_REF^{commit}") ||
    fail "RELEASE_REF is not a commit"
[ "$COMMIT" = "$(git -C "$ROOT" rev-parse HEAD)" ] ||
    fail "RELEASE_REF does not match the checked-out commit"
SOURCE_TREE=$(git -C "$ROOT" rev-parse "$COMMIT^{tree}") ||
    fail "cannot resolve source tree"

candidate=$CANDIDATE_DIR/candidate.json
checksums=$CANDIDATE_DIR/SHA256SUMS
[ -f "$candidate" ] && [ ! -L "$candidate" ] &&
    [ -f "$checksums" ] && [ ! -L "$checksums" ] ||
    fail "candidate metadata is missing"
(cd "$CANDIDATE_DIR" && shasum -a 256 -c SHA256SUMS) ||
    fail "candidate artifact hashes do not match"

evidence=$OUTPUT_DIR/hosted-validation-evidence.json
"$HAMN_DEV" release hosted-evidence "$CANDIDATE_DIR" "$evidence" "$RELEASE_TAG" \
    "$COMMIT" "$SOURCE_TREE" "$RUN_ID" "$RUN_ATTEMPT"
chmod 0644 "$evidence"
echo "bound hosted validation to exact candidate ${RELEASE_TAG}"
