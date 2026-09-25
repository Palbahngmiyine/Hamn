#!/bin/bash
# Validate exact RC bytes on a physical Apple Silicon runner. Never rebuild.
# The harness is hamn-dev, which `make release-gate` builds from this clean
# checkout; the checkout must be the candidate's own commit and source tree.
# The harness runs the candidate's archived executable, never a local build.
set -euo pipefail
export LC_ALL=C
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
fail() { echo "hamn release gate: $*" >&2; exit 1; }
[ -n "${RELEASE_REF:-}" ] && [ -n "${RELEASE_TAG:-}" ] && \
    [ -n "${CANDIDATE_DIR:-}" ] && [ -n "${OUTPUT_DIR:-}" ] || \
    fail 'RELEASE_REF, RELEASE_TAG, CANDIDATE_DIR, and OUTPUT_DIR are required'
[ -x "${HAMN_DEV:-}" ] || fail 'HAMN_DEV must name the hamn-dev built from this checkout'
[ "$(uname -m)" = arm64 ] || fail 'physical Apple Silicon validator required'
[ -z "$(git -C "$ROOT" status --porcelain)" ] || fail 'validator source tree is dirty'
commit=$(git -C "$ROOT" rev-parse --verify "$RELEASE_REF^{commit}")
[ "$commit" = "$(git -C "$ROOT" rev-parse HEAD)" ] || fail 'release commit differs from checkout'
tree=$(git -C "$ROOT" rev-parse "$commit^{tree}")
[ -d "$CANDIDATE_DIR" ] && [ ! -L "$CANDIDATE_DIR" ] || fail 'unsafe candidate directory'
"$HAMN_DEV" release validate-candidate "$CANDIDATE_DIR" "$RELEASE_TAG" "$commit" "$tree"
mkdir -p "$OUTPUT_DIR"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] || fail 'unsafe output directory'
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] || fail 'output directory must be empty'
HAMN_CANDIDATE_DIR="$CANDIDATE_DIR" \
HAMN_E2E_OUTPUT="$OUTPUT_DIR/physical-validation-evidence.json" \
    "$HAMN_DEV" release physical-e2e
"$HAMN_DEV" release validate-physical-evidence "$CANDIDATE_DIR/candidate.json" \
    "$CANDIDATE_DIR/SHA256SUMS" "$OUTPUT_DIR/physical-validation-evidence.json" \
    "${GITHUB_RUN_ID:-local}" "${GITHUB_RUN_ATTEMPT:-local}"
echo "validated exact candidate $RELEASE_TAG; physical evidence is in $OUTPUT_DIR"
