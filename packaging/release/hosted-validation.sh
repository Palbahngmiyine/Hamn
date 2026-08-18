#!/bin/bash
# Bind a GitHub-hosted regression result to exact candidate bytes. This does
# not claim that Virtualization.framework or a live Hamn VM was exercised.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn hosted validation: $*" >&2
    exit 1
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
RELEASE_REF=${RELEASE_REF:-}
RELEASE_TAG=${RELEASE_TAG:-}
CANDIDATE_DIR=${CANDIDATE_DIR:-}
OUTPUT_DIR=${OUTPUT_DIR:-}
RUN_ID=${GITHUB_RUN_ID:-local}
RUN_ATTEMPT=${GITHUB_RUN_ATTEMPT:-local}

[ -n "$RELEASE_REF" ] && [ -n "$RELEASE_TAG" ] &&
    [ -n "$CANDIDATE_DIR" ] && [ -n "$OUTPUT_DIR" ] ||
    fail "RELEASE_REF, RELEASE_TAG, CANDIDATE_DIR, and OUTPUT_DIR are required"
[[ "$RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+$ ]] ||
    fail "RELEASE_TAG must be a release candidate tag"
case "$RUN_ID:$RUN_ATTEMPT" in
local:local) ;;
*)
    [[ "$RUN_ID" =~ ^[1-9][0-9]*$ ]] &&
        [[ "$RUN_ATTEMPT" =~ ^[1-9][0-9]*$ ]] ||
        fail "workflow run and attempt must be positive decimals"
    ;;
esac
[ -d "$CANDIDATE_DIR" ] && [ ! -L "$CANDIDATE_DIR" ] ||
    fail "CANDIDATE_DIR is unsafe"
mkdir -p "$OUTPUT_DIR"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] ||
    fail "OUTPUT_DIR is unsafe"
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] ||
    fail "OUTPUT_DIR must be empty"

COMMIT=$(git -C "$ROOT" rev-parse --verify "$RELEASE_REF^{commit}") ||
    fail "RELEASE_REF is not a commit"
[ "$COMMIT" = "$(git -C "$ROOT" rev-parse HEAD)" ] ||
    fail "RELEASE_REF does not match the checked-out commit"
SOURCE_TREE=$(git -C "$ROOT" rev-parse "$COMMIT^{tree}") ||
    fail "cannot resolve source tree"

candidate=$CANDIDATE_DIR/candidate.json
checksums=$CANDIDATE_DIR/SHA256SUMS
[ -f "$candidate" ] && [ ! -L "$candidate" ] &&
    [ -f "$checksums" ] && [ ! -L "$checksums" ] ||
    fail "candidate metadata is missing"
(cd "$CANDIDATE_DIR" && shasum -a 256 -c SHA256SUMS) ||
    fail "candidate artifact hashes do not match"

evidence=$OUTPUT_DIR/hosted-validation-evidence.json
python3 - "$candidate" "$checksums" "$evidence" "$RELEASE_TAG" \
    "$COMMIT" "$SOURCE_TREE" "$RUN_ID" "$RUN_ATTEMPT" \
    "$(sha256_file "$candidate")" "$(sha256_file "$checksums")" <<'PY'
import json
import re
import sys

(candidate_path, checksums_path, evidence_path, tag, commit, tree, run,
 attempt, candidate_hash, checksums_hash) = sys.argv[1:]
with open(candidate_path, encoding="utf-8") as source:
    candidate = json.load(source)
if set(candidate) != {
        "schemaVersion", "kind", "tag", "version", "commit", "sourceTree",
        "artifacts"}:
    raise SystemExit("candidate schema is invalid")
if candidate["schemaVersion"] != 1 or \
        candidate["kind"] != "hamn-release-candidate" or \
        candidate["tag"] != tag or candidate["commit"] != commit or \
        candidate["sourceTree"] != tree:
    raise SystemExit("candidate identity does not match hosted validation")
if candidate["version"] != tag.rsplit("-rc.", 1)[0]:
    raise SystemExit("candidate version does not match release tag")
artifacts = candidate["artifacts"]
if not isinstance(artifacts, list) or len(artifacts) != 4:
    raise SystemExit("candidate artifact list is invalid")
artifact_map = {}
for item in artifacts:
    if not isinstance(item, dict) or set(item) != {"name", "sha256"} or \
            not isinstance(item["name"], str) or \
            not re.fullmatch(r"[A-Za-z0-9._-]+", item["name"]) or \
            not isinstance(item["sha256"], str) or \
            not re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) or \
            item["name"] in artifact_map:
        raise SystemExit("candidate artifact entry is invalid")
    artifact_map[item["name"]] = item["sha256"]
with open(checksums_path, encoding="utf-8") as source:
    checksum_names = {line.split(None, 1)[1].strip() for line in source if line.strip()}
if checksum_names != set(artifact_map) | {"candidate.json"}:
    raise SystemExit("candidate checksum set is incomplete")
evidence = {
    "schemaVersion": 1,
    "kind": "hamn-hosted-validation-evidence",
    "validationMode": "github-hosted-no-vm",
    "physicalE2E": False,
    "tag": tag,
    "commit": commit,
    "sourceTree": tree,
    "workflow": {"run": run, "attempt": attempt},
    "candidate": {
        "candidateJsonSha256": candidate_hash,
        "checksumsSha256": checksums_hash,
        "artifacts": artifact_map,
    },
    "checks": {
        "testLocalMacOS": True,
        "artifactHashes": True,
        "archiveSafety": True,
        "guestImageContract": True,
        "vmLifecycle": False,
        "dockerE2E": False,
        "k3sE2E": False,
        "colimaCoexistence": False,
    },
}
with open(evidence_path, "w", encoding="utf-8", newline="\n") as output:
    json.dump(evidence, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
chmod 0644 "$evidence"
echo "bound hosted validation to exact candidate ${RELEASE_TAG}"
