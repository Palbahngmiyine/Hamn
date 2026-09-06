#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-request.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

# Release tags in the developer checkout must not affect deterministic tests.
SOURCE=$ROOT
ROOT=$WORK/repo
mkdir -p "$ROOT/packaging/release"
cp "$SOURCE/packaging/release/resolve-release-request.sh" "$ROOT/packaging/release/"
# Recovery fixtures must not depend on the checkout's current release version.
printf '%s\n' '{".":"0.1.0"}' >"$ROOT/.release-please-manifest.json"
printf '0.1.0\n' >"$ROOT/version.txt"
printf 'VERSION ?= 0.1.0\n' >"$ROOT/Makefile"
printf 'hamnVersion = "0.1.0"; # x-release-please-version\n' >"$ROOT/flake.nix"
git -C "$ROOT" init -q
git -C "$ROOT" add .
git -C "$ROOT" -c user.name=Hamn-test -c user.email=test@example.invalid \
    -c commit.gpgsign=false commit -qm fixture
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
    'version=0.1.0' \
    'stable_tag=v0.1.0' \
    'candidate_tag=v0.1.0-rc.417123456' \
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

git -C "$ROOT" tag v0.1.0
: >"$output"
if GITHUB_EVENT_NAME=workflow_dispatch GITHUB_REF=refs/heads/main \
    GITHUB_SHA="$commit" GITHUB_RUN_ID=417123456 GITHUB_OUTPUT="$output" \
    bash "$ROOT/packaging/release/resolve-release-request.sh" \
    >"$WORK/tag.out" 2>"$WORK/tag.err"; then
    echo "FAIL: release recovery accepted an already published version" >&2
    exit 1
fi
grep -Fq 'stable tag already exists' "$WORK/tag.err"
[ ! -s "$output" ]

echo "PASS: unpublished release recovery is pinned to protected main"
