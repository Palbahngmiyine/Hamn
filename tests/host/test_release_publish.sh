#!/bin/bash
# Keyless promotion verifies hosted evidence and exact candidate bytes. It
# creates immutable-release metadata without rebuilding or private keys.
set -euo pipefail
unset GITHUB_ACTIONS GITHUB_REPOSITORY GITHUB_RUN_ID GITHUB_RUN_ATTEMPT

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
# Candidate builds replace build/hamn; restore the version this test found.
original_version=$("$ROOT/build/hamn" --version 2>/dev/null | awk '{print $2}') || original_version=
WORK=$(mktemp -d /tmp/hamn-release-publish.XXXXXX)
cleanup() {
    rm -rf "$WORK"
    [ -z "$original_version" ] || make -C "$ROOT" host VERSION="$original_version" >/dev/null
}
trap cleanup EXIT

sha256() {
    shasum -a 256 "$1" | awk '{print $1}'
}

release_ref=$(git -C "$ROOT" rev-parse HEAD)
source_tree=$(git -C "$ROOT" rev-parse HEAD^{tree})
workflow_run=417123456
workflow_attempt=2
repository=example/hamn
input=$WORK/input
candidate=$input/hamn-candidate
evidence=$input/hamn-evidence
mkdir -p "$candidate" "$evidence"
printf 'immutable guest image fixture\n' >"$WORK/guest.img"

GITHUB_REPOSITORY="$repository" \
RELEASE_REF="$release_ref" \
RELEASE_TAG=v0.0.1-rc.417123456 \
OUTPUT_DIR="$candidate" \
HAMN_GUEST_IMAGE="$WORK/guest.img" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    "$HAMN_DEV" release build-candidate >/dev/null

RELEASE_REF="$release_ref" \
RELEASE_TAG=v0.0.1-rc.417123456 \
CANDIDATE_DIR="$candidate" \
OUTPUT_DIR="$evidence" \
GITHUB_RUN_ID="$workflow_run" \
GITHUB_RUN_ATTEMPT="$workflow_attempt" \
    "$HAMN_DEV" release hosted-validation >/dev/null

host=$candidate/hamn-v0.0.1-darwin-arm64.tar.gz
host_hash=$(sha256 "$host")
# Synthetic fixture evidence exercises the gate; it is never production image
# measurement and is only accepted through the explicit local-test boundary.
image=$candidate/hamn-v0.0.1-ubuntu-24.04-arm64.img
image_bytes=$(stat -f %z "$image")
jq -n --argjson bytes "$image_bytes" --arg digest "$(sha256 "$image")" \
    --arg revision "$release_ref" --arg ones "$(printf '%064d' 0 | tr 0 1)" \
    --arg twos "$(printf '%064d' 0 | tr 0 2)" '{
        schemaVersion: 1, reviewOnly: false, compressedBytes: $bytes,
        imageSha256: $digest, baselineCompressedBytes: ($bytes + 64 * 1048576),
        baselineSha256: $ones, savedBytes: (64 * 1048576),
        requiredSavingsBytes: (64 * 1048576), virtualBytes: (8 * 1073741824),
        baseImageSha256: $twos, sourceRevision: $revision,
        packagesBefore: ["fixture"], packagesAfter: ["fixture"],
        cleanup: ["fixture only"], runtimeValidation: "not executed"}' \
    >"$evidence/guest-image-size-report.json"
jq -n --argjson bytes "$image_bytes" --arg digest "$(sha256 "$image")" \
    --arg report "$(sha256 "$evidence/guest-image-size-report.json")" '{
        schemaVersion: 1, maximumCompressedBytes: ($bytes + 1),
        referenceImageSha256: $digest, footprintReportSha256: $report}' \
    >"$WORK/size-budget.json"
export HAMN_RELEASE_ALLOW_LOCAL=1 HAMN_TEST_RELEASE_SIZE_BUDGET="$WORK/size-budget.json"
publish=$WORK/publish
mkdir "$publish"
HAMN_RELEASE_REPOSITORY="$repository" \
HAMN_EXPECTED_WORKFLOW_RUN="$workflow_run" \
HAMN_EXPECTED_WORKFLOW_ATTEMPT="$workflow_attempt" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.417123456 "$release_ref" "$input" "$publish" \
    >"$WORK/publish.out"

[ "$(sha256 "$host")" = "$host_hash" ]
[ ! -e "$publish/hamn-update-manifest.json.sig" ]
[ ! -e "$publish/validation-evidence.json.sig" ]
# Only the schema v3 manifest is published.
[ ! -e "$publish/hamn-update-manifest.json" ]
# Keep publisher URLs and bytes unchanged; native curl uses a bounded local TLS
# CONNECT fixture with child-only trust, not a PATH replacement.
HAMN_SOURCE_ROOT="$ROOT" HAMN_PUBLISHED_DIR="$publish" HAMN_CANDIDATE_DIR="$candidate" \
HAMN_CONSUMER_WORK="$WORK/consumers" \
    "$HAMN_DEV" test release-publisher-consumer
guest=$candidate/hamn-v0.0.1-ubuntu-24.04-arm64.img
jq -e --arg commit "$release_ref" \
    --arg base "https://github.com/example/hamn/releases/download/v0.0.1" \
    --arg host_hash "$host_hash" --argjson host_size "$(stat -f %z "$host")" \
    --arg guest_hash "$(sha256 "$guest")" --argjson guest_size "$(stat -f %z "$guest")" '
    (keys == ["artifacts", "channel", "commit", "compatibility", "schemaVersion",
        "validationMode", "version"]) and
    .schemaVersion == 3 and .channel == "stable" and .version == "v0.0.1" and
    .commit == $commit and .validationMode == "github-hosted-no-vm" and
    .compatibility == {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"} and
    .artifacts.host == {"url": ($base + "/hamn-v0.0.1-darwin-arm64.tar.gz"),
        "sha256": $host_hash, "size": $host_size} and
    .artifacts.guestImage == {"url": ($base + "/hamn-v0.0.1-ubuntu-24.04-arm64.img"),
        "sha256": $guest_hash, "size": $guest_size, "format": "qcow2",
        "compression": "zlib", "virtualSize": 8589934592}
' "$publish/hamn-update-manifest-v3.json" >/dev/null || {
    echo "FAIL: keyless update manifest does not bind the exact candidate" >&2
    exit 1
}
jq -e --arg tree "$source_tree" '.physicalE2E == false and .sourceTree == $tree' \
    "$publish/hosted-validation-evidence.json" >/dev/null || {
    echo "FAIL: hosted evidence overstates validation" >&2
    exit 1
}

# Missing reviewed evidence and review-only reports must fail at publication,
# even when all candidate and hosted-validation identities are otherwise valid.
cp "$evidence/guest-image-size-report.json" "$WORK/size-report.backup"
for size_failure in missing-budget review-only; do
    rejected=$WORK/$size_failure
    mkdir "$rejected"
    test_budget=$WORK/size-budget.json
    if [ "$size_failure" = missing-budget ]; then
        test_budget=$WORK/absent-budget.json
    else
        jq '.reviewOnly = true' "$WORK/size-report.backup" \
            >"$evidence/guest-image-size-report.json"
    fi
    if HAMN_TEST_RELEASE_SIZE_BUDGET="$test_budget" \
        HAMN_RELEASE_REPOSITORY="$repository" \
        HAMN_EXPECTED_WORKFLOW_RUN="$workflow_run" \
        HAMN_EXPECTED_WORKFLOW_ATTEMPT="$workflow_attempt" \
        bash "$ROOT/packaging/release/publish-release.sh" \
        v0.0.1 v0.0.1-rc.417123456 "$release_ref" "$input" "$rejected" \
        >"$WORK/$size_failure.out" 2>"$WORK/$size_failure.err"; then
        echo "FAIL: promotion accepted $size_failure image evidence" >&2
        exit 1
    fi
    grep -Fq 'guest image size evidence or reviewed release budget is missing or invalid' "$WORK/$size_failure.err"
    [ ! -e "$rejected/hamn-update-manifest-v3.json" ]
done
cp "$WORK/size-report.backup" "$evidence/guest-image-size-report.json"

wrong_run=$WORK/wrong-run
mkdir "$wrong_run"
if HAMN_RELEASE_REPOSITORY="$repository" \
    HAMN_EXPECTED_WORKFLOW_RUN=417123457 \
    HAMN_EXPECTED_WORKFLOW_ATTEMPT="$workflow_attempt" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.417123456 "$release_ref" "$input" "$wrong_run" \
    >"$WORK/wrong-run.out" 2>"$WORK/wrong-run.err"; then
    echo "FAIL: promotion accepted evidence from another workflow run" >&2
    exit 1
fi
grep -Fq 'hosted validation workflow provenance mismatch' "$WORK/wrong-run.err"

cp "$evidence/hosted-validation-evidence.json" "$WORK/evidence.backup"
jq -S -c '.physicalE2E = true' "$WORK/evidence.backup" \
    >"$evidence/hosted-validation-evidence.json"
overstated=$WORK/overstated
mkdir "$overstated"
if HAMN_RELEASE_REPOSITORY="$repository" \
    HAMN_EXPECTED_WORKFLOW_RUN="$workflow_run" \
    HAMN_EXPECTED_WORKFLOW_ATTEMPT="$workflow_attempt" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.417123456 "$release_ref" "$input" "$overstated" \
    >"$WORK/overstated.out" 2>"$WORK/overstated.err"; then
    echo "FAIL: promotion accepted false physical validation evidence" >&2
    exit 1
fi
grep -Fq 'hosted validation identity mismatch' "$WORK/overstated.err"
cp "$WORK/evidence.backup" "$evidence/hosted-validation-evidence.json"

printf 'unbound data\n' >"$candidate/unbound.txt"
extra=$WORK/extra
mkdir "$extra"
if HAMN_RELEASE_REPOSITORY="$repository" \
    HAMN_EXPECTED_WORKFLOW_RUN="$workflow_run" \
    HAMN_EXPECTED_WORKFLOW_ATTEMPT="$workflow_attempt" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.417123456 "$release_ref" "$input" "$extra" \
    >"$WORK/extra.out" 2>"$WORK/extra.err"; then
    echo "FAIL: promotion accepted an unbound candidate file" >&2
    exit 1
fi
grep -Fq 'candidate artifact directory contains unexpected entries' "$WORK/extra.err"

echo "PASS: keyless promotion verifies hosted evidence without rebuilding"
