#!/bin/bash
# Recover an unpublished manifest version from the current protected main.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn release request: $*" >&2
    exit 1
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
OUTPUT=${GITHUB_OUTPUT:-}
COMMIT=${GITHUB_SHA:-}

[ "${GITHUB_EVENT_NAME:-}" = workflow_dispatch ] ||
    fail "only workflow_dispatch may recover an unpublished release"
[ "${GITHUB_REF:-}" = refs/heads/main ] ||
    fail "release recovery must run from main"
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || fail "GITHUB_SHA must be a full commit SHA"
[ "$COMMIT" = "$(git -C "$ROOT" rev-parse HEAD)" ] ||
    fail "GITHUB_SHA does not match the checked-out commit"
[ -n "$OUTPUT" ] && [ -f "$OUTPUT" ] && [ ! -L "$OUTPUT" ] ||
    fail "GITHUB_OUTPUT must name an existing regular file"
[[ "${GITHUB_RUN_ID:-}" =~ ^[1-9][0-9]*$ ]] ||
    fail "GITHUB_RUN_ID must be a positive decimal"
[ -x "${HAMN_DEV:-}" ] || fail "HAMN_DEV must name the built hamn-dev executable"

version=$("$HAMN_DEV" release current-version "$ROOT") ||
    fail "cannot resolve the current release version"
stable_tag=v$version
if git -C "$ROOT" rev-parse --verify --quiet "refs/tags/$stable_tag" >/dev/null; then
    fail "stable tag already exists: $stable_tag"
fi
candidate_tag=$stable_tag-rc.${GITHUB_RUN_ID}
{
    printf 'should_release=true\n'
    printf 'version=%s\n' "$version"
    printf 'stable_tag=%s\n' "$stable_tag"
    printf 'candidate_tag=%s\n' "$candidate_tag"
    printf 'commit=%s\n' "$COMMIT"
} >>"$OUTPUT"
echo "recovered unpublished release $stable_tag from protected main"
