#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
# Candidate builds replace build/hamn; restore the version this test found.
original_version=$("$ROOT/build/hamn" --version 2>/dev/null | awk '{print $2}') || original_version=
WORK=$(mktemp -d /tmp/hamn-hosted-validation.XXXXXX)
cleanup() {
    rm -rf "$WORK"
    [ -z "$original_version" ] || make -C "$ROOT" host VERSION="$original_version" >/dev/null
}
trap cleanup EXIT

guest=$WORK/guest.img
candidate=$WORK/candidate
printf 'immutable guest image fixture\n' >"$guest"
release_ref=$(git -C "$ROOT" rev-parse HEAD)

GITHUB_REPOSITORY=example/hamn \
RELEASE_REF="$release_ref" \
RELEASE_TAG=v0.0.1-rc.417123456 \
OUTPUT_DIR="$candidate" \
HAMN_GUEST_IMAGE="$guest" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/build-candidate.sh" >/dev/null

evidence=$WORK/evidence
RELEASE_REF="$release_ref" \
RELEASE_TAG=v0.0.1-rc.417123456 \
CANDIDATE_DIR="$candidate" \
OUTPUT_DIR="$evidence" \
GITHUB_RUN_ID=417123456 \
GITHUB_RUN_ATTEMPT=2 \
    bash "$ROOT/packaging/release/hosted-validation.sh" >/dev/null

# Identity and the exact capability claims: hosted evidence never states a
# VM, Docker, Colima or physical run, and names no removed K3s check.
jq -e --arg commit "$release_ref" --arg tree "$(git -C "$ROOT" rev-parse HEAD^{tree})" '
    .kind == "hamn-hosted-validation-evidence" and
    .validationMode == "github-hosted-no-vm" and .physicalE2E == false and
    .commit == $commit and .sourceTree == $tree and
    .workflow == {"run": "417123456", "attempt": "2"} and
    .checks == {"testLocalMacOS": true, "artifactHashes": true,
        "archiveSafety": true, "guestImageContract": true,
        "vmLifecycle": false, "dockerE2E": false, "colimaCoexistence": false}
' "$evidence/hosted-validation-evidence.json" >/dev/null || {
    echo "FAIL: hosted validation evidence identity or capabilities are invalid" >&2
    exit 1
}

tampered=$candidate/hamn-v0.0.1-darwin-arm64.tar.gz
printf 'tampered\n' >>"$tampered"
if RELEASE_REF="$release_ref" RELEASE_TAG=v0.0.1-rc.417123456 \
    CANDIDATE_DIR="$candidate" OUTPUT_DIR="$WORK/tampered-evidence" \
    bash "$ROOT/packaging/release/hosted-validation.sh" \
    >"$WORK/tampered.out" 2>"$WORK/tampered.err"; then
    echo "FAIL: hosted validation accepted modified candidate bytes" >&2
    exit 1
fi
grep -Fq 'candidate artifact hashes do not match' "$WORK/tampered.err"

echo "PASS: hosted validation binds exact bytes without claiming physical E2E"
