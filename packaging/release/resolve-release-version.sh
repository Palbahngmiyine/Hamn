#!/bin/bash
# Resolve a Release Please manifest transition into one automated release run.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn automated release: $*" >&2
    exit 1
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
PREVIOUS_REF=${1:-}
OUTPUT=${GITHUB_OUTPUT:-}
RUN_ID=${GITHUB_RUN_ID:-}
COMMIT=${GITHUB_SHA:-}

[[ "$PREVIOUS_REF" =~ ^[0-9a-f]{40}$ ]] ||
    fail "previous release ref must be a full commit SHA"
[ -n "$OUTPUT" ] && [ -f "$OUTPUT" ] && [ ! -L "$OUTPUT" ] ||
    fail "GITHUB_OUTPUT must name an existing regular file"

if [ "$PREVIOUS_REF" = 0000000000000000000000000000000000000000 ] ||
        ! git -C "$ROOT" cat-file -e \
            "$PREVIOUS_REF:.release-please-manifest.json" 2>/dev/null; then
    printf 'should_release=false\n' >>"$OUTPUT"
    echo "Release Please bootstrap detected; no release is due"
    exit 0
fi

[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] ||
    fail "GITHUB_SHA must be a full commit SHA"
[[ "$RUN_ID" =~ ^[1-9][0-9]*$ ]] ||
    fail "GITHUB_RUN_ID must be a positive decimal integer"
[ "$COMMIT" = "$(git -C "$ROOT" rev-parse --verify HEAD)" ] ||
    fail "GITHUB_SHA does not match the checked-out commit"
[ -x "${HAMN_DEV:-}" ] || fail "HAMN_DEV must name the built hamn-dev executable"

"$HAMN_DEV" release resolve-version "$ROOT" "$PREVIOUS_REF" "$COMMIT" "$RUN_ID" "$OUTPUT"

echo "automated release version resolved from Release Please manifest"
