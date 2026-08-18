#!/bin/bash
# Promote exact GitHub-hosted candidate bytes without rebuilding or using a
# long-lived release key. The workflow verifies GitHub attestations separately.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn publish: $*" >&2
    exit 1
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

safe_regular() {
    local path=$1
    [ -f "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(stat -f '%u:%l' "$path")" = "$(id -u):1" ]
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
STABLE_TAG=${1:-}
RC_TAG=${2:-}
RELEASE_REF=${3:-}
INPUT_DIR=${4:-}
OUTPUT_DIR=${5:-}
EXPECTED_WORKFLOW_RUN=${HAMN_EXPECTED_WORKFLOW_RUN:-}
EXPECTED_WORKFLOW_ATTEMPT=${HAMN_EXPECTED_WORKFLOW_ATTEMPT:-}
PROVENANCE=${HAMN_RELEASE_PROVENANCE:-workflow}
RELEASE_REPOSITORY=${HAMN_RELEASE_REPOSITORY:-}
RELEASE_BASE_URL=${HAMN_RELEASE_BASE_URL:-}

[ -n "$STABLE_TAG" ] && [ -n "$RC_TAG" ] && [ -n "$RELEASE_REF" ] &&
    [ -n "$INPUT_DIR" ] && [ -n "$OUTPUT_DIR" ] ||
    fail "usage: publish-release.sh vX.Y.Z vX.Y.Z-rc.N COMMIT INPUT_DIR OUTPUT_DIR"
[[ "$STABLE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail "stable tag is invalid"
[[ "$RC_TAG" =~ ^${STABLE_TAG}-rc\.[0-9]+$ ]] ||
    fail "RC tag does not correspond to the stable tag"
case "$PROVENANCE" in
workflow)
    [[ "$EXPECTED_WORKFLOW_RUN" =~ ^[1-9][0-9]*$ ]] ||
        fail "HAMN_EXPECTED_WORKFLOW_RUN must be a positive decimal run ID"
    [[ "$EXPECTED_WORKFLOW_ATTEMPT" =~ ^[1-9][0-9]*$ ]] ||
        fail "HAMN_EXPECTED_WORKFLOW_ATTEMPT must be a positive decimal attempt"
    ;;
solo-local)
    [ "${GITHUB_ACTIONS:-}" != true ] ||
        fail "solo-local provenance is unavailable inside GitHub Actions"
    [ -z "$EXPECTED_WORKFLOW_RUN" ] && [ -z "$EXPECTED_WORKFLOW_ATTEMPT" ] ||
        fail "solo-local provenance must not accept workflow run inputs"
    EXPECTED_WORKFLOW_RUN=local
    EXPECTED_WORKFLOW_ATTEMPT=local
    ;;
*) fail "HAMN_RELEASE_PROVENANCE must be workflow or solo-local" ;;
esac
if [ -n "$RELEASE_REPOSITORY" ]; then
    [[ "$RELEASE_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
        fail "HAMN_RELEASE_REPOSITORY is invalid"
    [ -z "$RELEASE_BASE_URL" ] ||
        fail "HAMN_RELEASE_BASE_URL must not override the canonical GitHub Release base"
    BASE_URL="https://github.com/${RELEASE_REPOSITORY}/releases/download/${STABLE_TAG}"
else
    BASE_URL=$RELEASE_BASE_URL
    [ -n "$BASE_URL" ] || fail "HAMN_RELEASE_BASE_URL is required outside GitHub Actions"
    RELEASE_REPOSITORY=local/hamn
fi
case "$BASE_URL" in https://*) ;; *) fail "release base URL must use HTTPS" ;; esac

[ -d "$INPUT_DIR" ] && [ ! -L "$INPUT_DIR" ] || fail "INPUT_DIR is unsafe"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] || fail "OUTPUT_DIR is unsafe"
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] ||
    fail "OUTPUT_DIR must be empty"

CANDIDATE_DIR=$INPUT_DIR/hamn-candidate
EVIDENCE_DIR=$INPUT_DIR/hamn-evidence
[ -d "$CANDIDATE_DIR" ] && [ ! -L "$CANDIDATE_DIR" ] &&
    [ -d "$EVIDENCE_DIR" ] && [ ! -L "$EVIDENCE_DIR" ] ||
    fail "candidate or hosted evidence directory is missing"
version=${STABLE_TAG#v}
HOST_FILE="hamn-${STABLE_TAG}-darwin-arm64.tar.gz"
GUEST_FILE="hamn-${STABLE_TAG}-ubuntu-24.04-arm64.img"
SBOM_FILE="hamn-${STABLE_TAG}.spdx.json"
INSTALLER_FILE=install.sh
candidate=$CANDIDATE_DIR/candidate.json
checksums=$CANDIDATE_DIR/SHA256SUMS
evidence=$EVIDENCE_DIR/hosted-validation-evidence.json
for file in "$candidate" "$checksums" "$CANDIDATE_DIR/$HOST_FILE" \
    "$CANDIDATE_DIR/$GUEST_FILE" "$CANDIDATE_DIR/$SBOM_FILE" \
    "$CANDIDATE_DIR/$INSTALLER_FILE" "$evidence"; do
    safe_regular "$file" || fail "unsafe release input: $file"
done
(cd "$CANDIDATE_DIR" && shasum -a 256 -c SHA256SUMS) ||
    fail "candidate artifact hashes do not match"

COMMIT=$(git -C "$ROOT" rev-parse --verify "$RELEASE_REF^{commit}") ||
    fail "RELEASE_REF is not a commit"
SOURCE_TREE=$(git -C "$ROOT" rev-parse "$COMMIT^{tree}") ||
    fail "cannot resolve release source tree"
CANDIDATE_HASH=$(sha256_file "$candidate")
CHECKSUMS_HASH=$(sha256_file "$checksums")

python3 - "$CANDIDATE_DIR" "$candidate" "$evidence" "$STABLE_TAG" \
    "$RC_TAG" "$COMMIT" "$SOURCE_TREE" "$CANDIDATE_HASH" \
    "$CHECKSUMS_HASH" "$EXPECTED_WORKFLOW_RUN" \
    "$EXPECTED_WORKFLOW_ATTEMPT" "$HOST_FILE" "$GUEST_FILE" \
    "$SBOM_FILE" "$INSTALLER_FILE" <<'PY'
import json
import os
import stat
import sys

(directory, candidate_path, evidence_path, stable_tag, rc_tag, commit, tree,
 candidate_hash, checksums_hash, expected_run, expected_attempt, host, guest,
 sbom, installer) = sys.argv[1:]
expected_files = {host, guest, sbom, installer, "candidate.json", "SHA256SUMS"}
actual = set()
with os.scandir(directory) as entries:
    for entry in entries:
        info = entry.stat(follow_symlinks=False)
        if entry.is_symlink() or not stat.S_ISREG(info.st_mode):
            raise SystemExit("candidate artifact directory contains an unsafe entry")
        actual.add(entry.name)
if actual != expected_files:
    raise SystemExit("candidate artifact directory contains unexpected entries")
with open(candidate_path, encoding="utf-8") as source:
    candidate = json.load(source)
with open(evidence_path, encoding="utf-8") as source:
    evidence = json.load(source)
if candidate.get("tag") != rc_tag or candidate.get("version") != stable_tag or \
        candidate.get("commit") != commit or candidate.get("sourceTree") != tree:
    raise SystemExit("candidate provenance mismatch")
artifacts = candidate.get("artifacts")
if not isinstance(artifacts, list):
    raise SystemExit("candidate artifacts are invalid")
artifact_map = {item.get("name"): item.get("sha256") for item in artifacts
                if isinstance(item, dict)}
if set(artifact_map) != {host, guest, sbom, installer}:
    raise SystemExit("candidate artifact names are invalid")
if set(evidence) != {
        "schemaVersion", "kind", "validationMode", "physicalE2E", "tag",
        "commit", "sourceTree", "workflow", "candidate", "checks"}:
    raise SystemExit("hosted validation evidence schema is invalid")
if evidence["schemaVersion"] != 1 or \
        evidence["kind"] != "hamn-hosted-validation-evidence" or \
        evidence["validationMode"] != "github-hosted-no-vm" or \
        evidence["physicalE2E"] is not False or evidence["tag"] != rc_tag or \
        evidence["commit"] != commit or evidence["sourceTree"] != tree:
    raise SystemExit("hosted validation identity mismatch")
if evidence.get("workflow") != {
        "run": expected_run, "attempt": expected_attempt}:
    raise SystemExit("hosted validation workflow provenance mismatch")
bound = evidence.get("candidate")
if not isinstance(bound, dict) or \
        bound.get("candidateJsonSha256") != candidate_hash or \
        bound.get("checksumsSha256") != checksums_hash or \
        bound.get("artifacts") != artifact_map:
    raise SystemExit("hosted validation candidate binding mismatch")
checks = evidence.get("checks")
if not isinstance(checks, dict) or checks.get("testLocalMacOS") is not True or \
        checks.get("artifactHashes") is not True or \
        checks.get("archiveSafety") is not True or \
        checks.get("guestImageContract") is not True or \
        any(checks.get(name) is not False for name in (
            "vmLifecycle", "dockerE2E", "k3sE2E", "colimaCoexistence")):
    raise SystemExit("hosted validation capabilities are invalid")
PY

HOST_HASH=$(sha256_file "$CANDIDATE_DIR/$HOST_FILE")
GUEST_HASH=$(sha256_file "$CANDIDATE_DIR/$GUEST_FILE")
MANIFEST=$OUTPUT_DIR/hamn-update-manifest.json
python3 - "$MANIFEST" "$version" "$RELEASE_REPOSITORY" "$COMMIT" \
    "$BASE_URL" "$HOST_FILE" "$HOST_HASH" "$GUEST_FILE" "$GUEST_HASH" <<'PY'
import json
import sys

(path, version, repository, commit, base, host_name, host_hash, guest_name,
 guest_hash) = sys.argv[1:]
value = {
    "schemaVersion": 2,
    "channel": "stable",
    "version": "v" + version,
    "repository": repository,
    "commit": commit,
    "validationMode": "github-hosted-no-vm",
    "compatibility": {
        "os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
    "artifacts": {
        "host": {"url": base + "/" + host_name, "sha256": host_hash},
        "guestImage": {"url": base + "/" + guest_name, "sha256": guest_hash},
    },
}
with open(path, "w", encoding="utf-8", newline="\n") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
cp "$candidate" "$OUTPUT_DIR/candidate.json"
cp "$checksums" "$OUTPUT_DIR/SHA256SUMS"
cp "$evidence" "$OUTPUT_DIR/hosted-validation-evidence.json"
printf '%s\n' "$RC_TAG" >"$OUTPUT_DIR/promoted-from-rc"
chmod 0644 "$OUTPUT_DIR"/*
echo "verified hosted candidate ${RC_TAG}; publish exact bytes without rebuilding"
