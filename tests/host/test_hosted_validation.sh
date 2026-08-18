#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-hosted-validation.XXXXXX)
cleanup() {
    rm -rf "$WORK"
    make -C "$ROOT" host VERSION=0.0.1-dev >/dev/null
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

python3 - "$evidence/hosted-validation-evidence.json" \
    "$release_ref" "$(git -C "$ROOT" rev-parse HEAD^{tree})" <<'PY'
import json
import sys

path, commit, tree = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    evidence = json.load(source)
if evidence.get("kind") != "hamn-hosted-validation-evidence" or \
        evidence.get("validationMode") != "github-hosted-no-vm" or \
        evidence.get("physicalE2E") is not False or \
        evidence.get("commit") != commit or evidence.get("sourceTree") != tree or \
        evidence.get("workflow") != {"run": "417123456", "attempt": "2"}:
    raise SystemExit("hosted validation evidence identity is invalid")
checks = evidence.get("checks")
if not isinstance(checks, dict) or checks.get("testLocalMacOS") is not True or \
        checks.get("artifactHashes") is not True or \
        checks.get("vmLifecycle") is not False or \
        checks.get("dockerE2E") is not False or checks.get("k3sE2E") is not False:
    raise SystemExit("hosted validation capabilities are overstated")
PY

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
