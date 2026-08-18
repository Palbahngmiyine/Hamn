#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-version.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

REPO=$WORK/repository
mkdir -p "$REPO/packaging/release"
cp "$ROOT/packaging/release/resolve-release-version.sh" \
    "$REPO/packaging/release/resolve-release-version.sh"
git -C "$REPO" init -q
git -C "$REPO" config user.name 'Hamn Test'
git -C "$REPO" config user.email 'hamn-test@invalid'

write_sources() {
    local version=$1
    printf '%s\n' "$version" >"$REPO/version.txt"
    printf '# x-release-please-start-version\nVERSION    ?= %s\n# x-release-please-end\n' \
        "$version" >"$REPO/Makefile"
    printf '      hamnVersion = "%s"; # x-release-please-version\n' "$version" \
        >"$REPO/flake.nix"
}

commit_all() {
    git -C "$REPO" add .
    git -C "$REPO" commit -q -m "$1"
    git -C "$REPO" rev-parse HEAD
}

run_resolver() {
    local previous=$1 output=$2
    : >"$output"
    GITHUB_OUTPUT="$output" \
    GITHUB_RUN_ID=417123456 \
    GITHUB_SHA=$(git -C "$REPO" rev-parse HEAD) \
        bash "$REPO/packaging/release/resolve-release-version.sh" "$previous"
}

write_sources 0.0.1
BASE=$(commit_all 'base without Release Please')
printf '%s\n' '{".":"0.0.0"}' >"$REPO/.release-please-manifest.json"
BOOTSTRAP=$(commit_all 'bootstrap Release Please')
run_resolver "$BASE" "$WORK/bootstrap.output"
grep -Fxq 'should_release=false' "$WORK/bootstrap.output"

printf '%s\n' '{".":"0.0.1"}' >"$REPO/.release-please-manifest.json"
RELEASE=$(commit_all 'release 0.0.1')
run_resolver "$BOOTSTRAP" "$WORK/release.output"
for expected in \
    'should_release=true' \
    'version=0.0.1' \
    'stable_tag=v0.0.1' \
    'candidate_tag=v0.0.1-rc.417123456' \
    "commit=$RELEASE"; do
    grep -Fxq "$expected" "$WORK/release.output"
done

write_sources 0.0.2
printf '%s\n' '{".":"0.0.1"}' >"$REPO/.release-please-manifest.json"
MISMATCH=$(commit_all 'mismatched source version')
if run_resolver "$BOOTSTRAP" "$WORK/mismatch.output" \
    >"$WORK/mismatch.out" 2>"$WORK/mismatch.err"; then
    echo "FAIL: release resolver accepted mismatched source versions" >&2
    exit 1
fi
grep -Fq 'version.txt does not match the release manifest' "$WORK/mismatch.err"

write_sources 0.0.1
printf '%s\n' '{".":"0.0.0"}' >"$REPO/.release-please-manifest.json"
ROLLBACK=$(commit_all 'non-increasing release')
if run_resolver "$BOOTSTRAP" "$WORK/rollback.output" \
    >"$WORK/rollback.out" 2>"$WORK/rollback.err"; then
    echo "FAIL: release resolver accepted a non-increasing version" >&2
    exit 1
fi
grep -Fq 'release version did not increase' "$WORK/rollback.err"

: >"$WORK/missing-run.output"
if GITHUB_OUTPUT="$WORK/missing-run.output" GITHUB_RUN_ID= \
GITHUB_SHA="$RELEASE" \
    bash "$REPO/packaging/release/resolve-release-version.sh" "$BOOTSTRAP" \
    >"$WORK/missing-run.out" 2>"$WORK/missing-run.err"; then
    echo "FAIL: release resolver accepted a missing workflow run ID" >&2
    exit 1
fi
grep -Fq 'GITHUB_RUN_ID must be a positive decimal integer' \
    "$WORK/missing-run.err"

echo "PASS: Release Please manifest transitions gate automated releases"
