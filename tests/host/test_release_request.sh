#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-request.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

commit=$(git -C "$ROOT" rev-parse HEAD)
output=$WORK/output
: >"$output"
GITHUB_EVENT_NAME=workflow_dispatch \
GITHUB_REF=refs/heads/main \
GITHUB_SHA="$commit" \
GITHUB_RUN_ID=417123456 \
GITHUB_OUTPUT="$output" \
    bash "$ROOT/packaging/release/resolve-release-request.sh" >/dev/null
for expected in \
    'should_release=true' \
    'version=0.0.1' \
    'stable_tag=v0.0.1' \
    'candidate_tag=v0.0.1-rc.417123456' \
    "commit=$commit"; do
    grep -Fxq "$expected" "$output"
done

: >"$WORK/wrong-ref.output"
if GITHUB_EVENT_NAME=workflow_dispatch GITHUB_REF=refs/heads/feature \
    GITHUB_SHA="$commit" GITHUB_RUN_ID=417123456 \
    GITHUB_OUTPUT="$WORK/wrong-ref.output" \
    bash "$ROOT/packaging/release/resolve-release-request.sh" \
    >"$WORK/wrong-ref.out" 2>"$WORK/wrong-ref.err"; then
    echo "FAIL: release recovery accepted a non-main ref" >&2
    exit 1
fi
grep -Fq 'release recovery must run from main' "$WORK/wrong-ref.err"

echo "PASS: unpublished release recovery is pinned to protected main"
