#!/bin/bash
# Stable promotion signs only a manifest after checking the exact RC candidate
# and validator evidence; it never recompiles or replaces candidate bytes.
set -euo pipefail
unset GITHUB_ACTIONS GITHUB_REPOSITORY GITHUB_RUN_ID GITHUB_RUN_ATTEMPT

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-publish.XXXXXX)
cleanup() {
    rm -rf "$WORK"
    make -C "$ROOT" host VERSION=0.0.1-dev >/dev/null
}
trap cleanup EXIT

command -v ssh-keygen >/dev/null || {
    echo "SKIP: ssh-keygen is unavailable" >&2
    exit 0
}

sha256() {
    shasum -a 256 "$1" | awk '{print $1}'
}

ssh-keygen -q -t ed25519 -N '' -f "$WORK/release-key"
ssh-keygen -q -t ed25519 -N '' -f "$WORK/validator-key"
ssh-keygen -q -t ed25519 -N '' -f "$WORK/wrong-release-key"
printf 'guest image fixture\n' >"$WORK/guest.img"
RELEASE_REF=$(git -C "$ROOT" rev-parse HEAD)
WORKFLOW_RUN=417123456
WORKFLOW_ATTEMPT=2
RELEASE_REPOSITORY=example/hamn
RELEASE_BASE_URL="https://github.com/$RELEASE_REPOSITORY/releases/download/v0.0.1"
INPUT=$WORK/input
mkdir -p "$INPUT/hamn-candidate"
GITHUB_REPOSITORY="$RELEASE_REPOSITORY" \
RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
OUTPUT_DIR="$INPUT/hamn-candidate" \
HAMN_GUEST_IMAGE="$WORK/guest.img" \
HAMN_RELEASE_PUBLIC_KEY="$WORK/release-key.pub" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/build-candidate.sh" >"$WORK/candidate.out"

HARNESS=$WORK/harness
mkdir -p "$HARNESS"
cat >"$HARNESS/e2e" <<'EOF'
#!/bin/bash
set -euo pipefail
candidate_hash=$(shasum -a 256 "$HAMN_CANDIDATE_JSON" | awk '{print $1}')
host_hash=$(awk '$2 ~ /darwin-arm64.tar.gz$/ { print $1 }' "$HAMN_CANDIDATE_DIR/SHA256SUMS")
guest_hash=$(awk '$2 ~ /ubuntu-24.04-arm64.img$/ { print $1 }' "$HAMN_CANDIDATE_DIR/SHA256SUMS")
printf '%s\n' '{"schemaVersion":1,"kind":"hamn-physical-e2e","passed":true,"tests":{"lifecycle":true,"multiProfile":true,"staleSocket":true,"dockerContextRestore":true,"mount":true,"network":true,"tcpPort":true,"udpPort":true,"amd64":true,"rosetta":true,"dockerCli":true,"compose":true,"buildx":true,"dockerSdkGo":true,"dockerSdkPython":true,"testcontainersJava":true,"testcontainersGo":true,"testcontainersPython":true,"testcontainersNode":true,"k3s":true,"updateRollback":true,"softDeleteRecovery":true,"hardDelete":true,"uninstall":true,"colimaCoexistence":true},"provenance":{"candidateJsonSha256":"'"$candidate_hash"'","hostArtifactSha256":"'"$host_hash"'","guestImageSha256":"'"$guest_hash"'"},"colima":{"beforeSha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","afterSha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","binaryBeforeSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","binaryAfterSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","instancesBeforeSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","instancesAfterSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}' >"$HAMN_E2E_OUTPUT"
EOF
chmod 0755 "$HARNESS/e2e"

mkdir "$INPUT/hamn-evidence"
RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
CANDIDATE_DIR="$INPUT/hamn-candidate" \
OUTPUT_DIR="$INPUT/hamn-evidence" \
HAMN_VALIDATOR_SIGNING_KEY="$WORK/validator-key" \
HAMN_VALIDATOR_IDENTITY=test-validator \
HAMN_GUEST_E2E_COMMAND="$HARNESS/e2e" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
HAMN_RELEASE_TEST_FIXTURES=1 \
GITHUB_RUN_ID="$WORKFLOW_RUN" \
GITHUB_RUN_ATTEMPT="$WORKFLOW_ATTEMPT" \
    bash "$ROOT/packaging/release/release-gate.sh" >"$WORK/gate.out"

PUBLISH=$WORK/publish
mkdir "$PUBLISH"
HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_EXPECTED_WORKFLOW_RUN="$WORKFLOW_RUN" \
HAMN_EXPECTED_WORKFLOW_ATTEMPT="$WORKFLOW_ATTEMPT" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$INPUT" "$PUBLISH" \
    >"$WORK/publish.out"

grep -Fq '"version":"v0.0.1"' "$PUBLISH/hamn-update-manifest.json"
grep -Fq "$RELEASE_BASE_URL/hamn-v0.0.1-darwin-arm64.tar.gz" \
    "$PUBLISH/hamn-update-manifest.json"
allowed=$WORK/allowed-signers
printf 'hamn-release ' >"$allowed"
cat "$WORK/release-key.pub" >>"$allowed"
ssh-keygen -Y verify -f "$allowed" -I hamn-release -n hamn-release \
    -s "$PUBLISH/hamn-update-manifest.json.sig" \
    <"$PUBLISH/hamn-update-manifest.json" >/dev/null
candidate_host=$INPUT/hamn-candidate/hamn-v0.0.1-darwin-arm64.tar.gz
[ "$(sha256 "$candidate_host")" = \
  "$(awk '$2 ~ /darwin-arm64.tar.gz$/ { print $1 }' "$INPUT/hamn-candidate/SHA256SUMS")" ]
[ "$(sha256 "$PUBLISH/e2e.json")" = \
  "$(sha256 "$INPUT/hamn-evidence/e2e.json")" ]

LOCAL_INPUT=$WORK/local-input
mkdir -p "$LOCAL_INPUT"
cp -R "$INPUT/hamn-candidate" "$LOCAL_INPUT/hamn-candidate"
mkdir "$LOCAL_INPUT/hamn-evidence"
RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
CANDIDATE_DIR="$LOCAL_INPUT/hamn-candidate" \
OUTPUT_DIR="$LOCAL_INPUT/hamn-evidence" \
HAMN_VALIDATOR_SIGNING_KEY="$WORK/validator-key" \
HAMN_VALIDATOR_IDENTITY=test-validator \
HAMN_GUEST_E2E_COMMAND="$HARNESS/e2e" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
HAMN_RELEASE_TEST_FIXTURES=1 \
    bash "$ROOT/packaging/release/release-gate.sh" >"$WORK/local-gate.out"
grep -Fq '"workflow":{"attempt":"local","run":"local"}' \
    "$LOCAL_INPUT/hamn-evidence/validation-evidence.json"

LOCAL_PUBLISH=$WORK/local-publish
mkdir "$LOCAL_PUBLISH"
HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_RELEASE_PROVENANCE=solo-local \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$LOCAL_INPUT" "$LOCAL_PUBLISH" \
    >"$WORK/local-publish.out"
grep -Fq '"version":"v0.0.1"' "$LOCAL_PUBLISH/hamn-update-manifest.json"

LOCAL_WITH_RUN=$WORK/local-with-run
mkdir "$LOCAL_WITH_RUN"
if HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_RELEASE_PROVENANCE=solo-local \
HAMN_EXPECTED_WORKFLOW_RUN=417123456 \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$LOCAL_INPUT" "$LOCAL_WITH_RUN" \
    >"$WORK/local-with-run.out" 2>"$WORK/local-with-run.err"; then
    echo "FAIL: solo-local publish accepted a workflow run input" >&2
    exit 1
fi
grep -Fq 'solo-local provenance must not accept workflow run inputs' \
    "$WORK/local-with-run.err"

LOCAL_IN_ACTIONS=$WORK/local-in-actions
mkdir "$LOCAL_IN_ACTIONS"
if GITHUB_ACTIONS=true \
HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_RELEASE_PROVENANCE=solo-local \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$LOCAL_INPUT" "$LOCAL_IN_ACTIONS" \
    >"$WORK/local-in-actions.out" 2>"$WORK/local-in-actions.err"; then
    echo "FAIL: solo-local publish ran inside GitHub Actions" >&2
    exit 1
fi
grep -Fq 'solo-local provenance is unavailable inside GitHub Actions' \
    "$WORK/local-in-actions.err"

PUBLISH_MISMATCH=$WORK/publish-mismatch
mkdir "$PUBLISH_MISMATCH"
if HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/wrong-release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_EXPECTED_WORKFLOW_RUN="$WORKFLOW_RUN" \
HAMN_EXPECTED_WORKFLOW_ATTEMPT="$WORKFLOW_ATTEMPT" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$INPUT" "$PUBLISH_MISMATCH" \
    >"$WORK/mismatch.out" 2>"$WORK/mismatch.err"; then
    echo "FAIL: publish accepted a signing key different from the candidate key" >&2
    exit 1
fi
grep -Fq 'candidate installer release key does not match release signing key' \
    "$WORK/mismatch.err"

PUBLISH_RUN_MISMATCH=$WORK/publish-run-mismatch
mkdir "$PUBLISH_RUN_MISMATCH"
if HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_EXPECTED_WORKFLOW_RUN=417123457 \
HAMN_EXPECTED_WORKFLOW_ATTEMPT="$WORKFLOW_ATTEMPT" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$INPUT" "$PUBLISH_RUN_MISMATCH" \
    >"$WORK/run-mismatch.out" 2>"$WORK/run-mismatch.err"; then
    echo "FAIL: publish accepted evidence from a different workflow run" >&2
    exit 1
fi
grep -Fq 'validation workflow provenance does not match verified RC run and attempt' \
    "$WORK/run-mismatch.err"

PUBLISH_ATTEMPT_MISMATCH=$WORK/publish-attempt-mismatch
mkdir "$PUBLISH_ATTEMPT_MISMATCH"
if HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_EXPECTED_WORKFLOW_RUN="$WORKFLOW_RUN" \
HAMN_EXPECTED_WORKFLOW_ATTEMPT=3 \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$INPUT" "$PUBLISH_ATTEMPT_MISMATCH" \
    >"$WORK/attempt-mismatch.out" 2>"$WORK/attempt-mismatch.err"; then
    echo "FAIL: publish accepted evidence from a different workflow attempt" >&2
    exit 1
fi
grep -Fq 'validation workflow provenance does not match verified RC run and attempt' \
    "$WORK/attempt-mismatch.err"

printf 'unbound candidate data\n' >"$INPUT/hamn-candidate/hamn-v0.0.1-unbound.tar.gz"
PUBLISH_EXTRA_FILE=$WORK/publish-extra-file
mkdir "$PUBLISH_EXTRA_FILE"
if HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_EXPECTED_WORKFLOW_RUN="$WORKFLOW_RUN" \
HAMN_EXPECTED_WORKFLOW_ATTEMPT="$WORKFLOW_ATTEMPT" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$INPUT" "$PUBLISH_EXTRA_FILE" \
    >"$WORK/extra-file.out" 2>"$WORK/extra-file.err"; then
    echo "FAIL: publish accepted a candidate directory with an unbound file" >&2
    exit 1
fi
grep -Fq 'candidate artifact directory contains unexpected entries' \
    "$WORK/extra-file.err"

PUBLISH_BASE_OVERRIDE=$WORK/publish-base-override
mkdir "$PUBLISH_BASE_OVERRIDE"
if HAMN_VALIDATOR_PUBLIC_KEY="$WORK/validator-key.pub" \
HAMN_RELEASE_SIGNING_KEY="$WORK/release-key" \
HAMN_RELEASE_REPOSITORY="$RELEASE_REPOSITORY" \
HAMN_RELEASE_BASE_URL=https://downloads.example.invalid/hamn/v0.0.1 \
HAMN_EXPECTED_WORKFLOW_RUN="$WORKFLOW_RUN" \
HAMN_EXPECTED_WORKFLOW_ATTEMPT="$WORKFLOW_ATTEMPT" \
    bash "$ROOT/packaging/release/publish-release.sh" \
    v0.0.1 v0.0.1-rc.1 "$RELEASE_REF" "$INPUT" "$PUBLISH_BASE_OVERRIDE" \
    >"$WORK/base-override.out" 2>"$WORK/base-override.err"; then
    echo "FAIL: publish accepted an override for the canonical GitHub Release base" >&2
    exit 1
fi
grep -Fq 'HAMN_RELEASE_BASE_URL must not override the canonical GitHub Release base' \
    "$WORK/base-override.err"

echo "PASS: stable promotion verifies signed RC evidence without rebuilding"
