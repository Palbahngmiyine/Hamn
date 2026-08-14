#!/bin/bash
# Validate exact candidate bytes on a physical Apple Silicon Mac and produce
# signed evidence. This script deliberately refuses to pretend hosted runners
# performed Virtualization.framework E2E.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn release gate: $*" >&2
    exit 1
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

safe_harness() {
    local path=$1
    [ "${path#/}" != "$path" ] && [ -f "$path" ] && [ ! -L "$path" ] &&
        [ -x "$path" ] && [ "$(stat -f '%u:%l' "$path")" = "$(id -u):1" ]
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
RELEASE_REF=${RELEASE_REF:-}
RELEASE_TAG=${RELEASE_TAG:-}
CANDIDATE_DIR=${CANDIDATE_DIR:-}
OUTPUT_DIR=${OUTPUT_DIR:-}
VALIDATOR_KEY=${HAMN_VALIDATOR_SIGNING_KEY:-}
VALIDATOR_IDENTITY=${HAMN_VALIDATOR_IDENTITY:-}
GUEST_E2E=${HAMN_GUEST_E2E_COMMAND:-}
ALLOW_DIRTY=${HAMN_RELEASE_ALLOW_DIRTY:-0}
ALLOW_TEST_FIXTURES=${HAMN_RELEASE_TEST_FIXTURES:-0}

[ -n "$RELEASE_REF" ] && [ -n "$RELEASE_TAG" ] && [ -n "$CANDIDATE_DIR" ] &&
    [ -n "$OUTPUT_DIR" ] ||
    fail "RELEASE_REF, RELEASE_TAG, CANDIDATE_DIR, and OUTPUT_DIR are required"
[ "${GITHUB_ACTIONS:-}" != true ] ||
    fail "release validation is unavailable inside GitHub Actions"
[ -z "${GITHUB_RUN_ID:-}" ] && [ -z "${GITHUB_RUN_ATTEMPT:-}" ] ||
    fail "release validation must not accept workflow run inputs"
[[ "$RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+$ ]] ||
    fail "RELEASE_TAG must be a release candidate tag"
[ "$(uname -m)" = arm64 ] || fail "validator must be Apple Silicon"
if ! python3 - "$(sw_vers -productVersion)" <<'PY'
import re
import sys

value = sys.argv[1]
match = re.fullmatch(r"([0-9]+)(?:\.[0-9]+){0,2}", value)
if not match or int(match.group(1)) < 13:
    raise SystemExit(1)
PY
then
    fail "validator must run macOS 13 or later"
fi
[ -d "$CANDIDATE_DIR" ] && [ ! -L "$CANDIDATE_DIR" ] ||
    fail "CANDIDATE_DIR is unsafe"
[ -n "$VALIDATOR_KEY" ] && [ -f "$VALIDATOR_KEY" ] && [ ! -L "$VALIDATOR_KEY" ] ||
    fail "HAMN_VALIDATOR_SIGNING_KEY must name one private key file"
[ -n "$VALIDATOR_IDENTITY" ] || fail "HAMN_VALIDATOR_IDENTITY is required"
case "$ALLOW_TEST_FIXTURES" in
0|1) ;;
*) fail "HAMN_RELEASE_TEST_FIXTURES must be 0 or 1" ;;
esac
if [ "$ALLOW_TEST_FIXTURES" != 1 ] && [ -n "$GUEST_E2E" ]; then
    fail "HAMN_GUEST_E2E_COMMAND is allowed only for test fixtures"
fi
if [ "$ALLOW_TEST_FIXTURES" != 1 ] && [ "$ALLOW_DIRTY" = 1 ]; then
    fail "HAMN_RELEASE_ALLOW_DIRTY is allowed only for test fixtures"
fi
if [ "$ALLOW_DIRTY" != 1 ] && [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
    fail "validator source tree is dirty"
fi

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
python3 - "$candidate" "$checksums" "$RELEASE_TAG" "$COMMIT" "$SOURCE_TREE" <<'PY'
import json
import re
import sys

path, checksums_path, tag, commit, tree = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
if set(value) != {"schemaVersion", "kind", "tag", "version", "commit", "sourceTree", "artifacts"}:
    raise SystemExit("candidate schema is invalid")
if value["schemaVersion"] != 1 or value["kind"] != "hamn-release-candidate":
    raise SystemExit("candidate identity is invalid")
if value["tag"] != tag or value["commit"] != commit or value["sourceTree"] != tree:
    raise SystemExit("candidate provenance does not match this validator")
if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", value["version"]):
    raise SystemExit("candidate version is invalid")
if value["version"] != tag.rsplit("-rc.", 1)[0]:
    raise SystemExit("candidate version does not match the RC tag")
if not isinstance(value["artifacts"], list) or len(value["artifacts"]) < 4:
    raise SystemExit("candidate artifact list is invalid")
artifact_names = set()
for item in value["artifacts"]:
    if set(item) != {"name", "sha256"} or not isinstance(item["name"], str) or \
            not re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) or \
            not re.fullmatch(r"[A-Za-z0-9._-]+", item["name"]) or \
            item["name"] in artifact_names:
        raise SystemExit("candidate artifact entry is invalid")
    artifact_names.add(item["name"])

expected_names = {
    "hamn-" + value["version"] + "-darwin-arm64.tar.gz",
    "hamn-" + value["version"] + "-ubuntu-24.04-arm64.img",
    "hamn-" + value["version"] + ".spdx.json",
    "install.sh",
}
if artifact_names != expected_names:
    raise SystemExit("candidate artifact names are invalid")

checksum_entries = {}
with open(checksums_path, encoding="utf-8") as source:
    for line in source:
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9._-]+)\n", line)
        if not match or match.group(2) in checksum_entries:
            raise SystemExit("SHA256SUMS is invalid")
        checksum_entries[match.group(2)] = match.group(1)
if set(checksum_entries) != expected_names | {"candidate.json"}:
    raise SystemExit("SHA256SUMS does not describe the exact candidate set")
if any(checksum_entries[item["name"]] != item["sha256"] for item in value["artifacts"]):
    raise SystemExit("candidate metadata does not match SHA256SUMS")
PY
CANDIDATE_HASH=$(sha256_file "$candidate")
HOST_ARTIFACT_NAME=$(python3 - "$candidate" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as source:
    candidate = json.load(source)
for item in candidate["artifacts"]:
    if item["name"].endswith("-darwin-arm64.tar.gz"):
        print(item["name"])
        break
else:
    raise SystemExit("candidate host artifact is missing")
PY
)
HOST_ARTIFACT_HASH=$(python3 - "$candidate" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as source:
    candidate = json.load(source)
for item in candidate["artifacts"]:
    if item["name"].endswith("-darwin-arm64.tar.gz"):
        print(item["sha256"])
        break
else:
    raise SystemExit("candidate host artifact is missing")
PY
)
GUEST_IMAGE_HASH=$(python3 - "$candidate" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as source:
    candidate = json.load(source)
for item in candidate["artifacts"]:
    if item["name"].endswith("-ubuntu-24.04-arm64.img"):
        print(item["sha256"])
        break
else:
    raise SystemExit("candidate guest image artifact is missing")
PY
)

mkdir -p "$OUTPUT_DIR"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] ||
    fail "OUTPUT_DIR is unsafe"
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] ||
    fail "OUTPUT_DIR must be empty"
WORK=$(mktemp -d "$OUTPUT_DIR/.hamn-release-gate.XXXXXX") ||
    fail "cannot create gate workspace"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

# The default harnesses are unpacked from the exact host candidate. Test-only
# overrides remain possible, but they must still be owned absolute executables.
if [ -z "$GUEST_E2E" ]; then
    CANDIDATE_HARNESS_ROOT=$(python3 - \
        "$CANDIDATE_DIR/$HOST_ARTIFACT_NAME" "$WORK/candidate-harness" \
        "$HOST_ARTIFACT_NAME" <<'PY'
import os
import posixpath
import sys
import tarfile

archive, destination, name = sys.argv[1:]
if not name.endswith(".tar.gz"):
    raise SystemExit("candidate host archive name is invalid")
expected_root = name[:-len(".tar.gz")]
with tarfile.open(archive, "r:gz") as bundle:
    members = bundle.getmembers()
    if not members:
        raise SystemExit("candidate host archive is empty")
    roots = set()
    actual = set()
    for member in members:
        path = member.name
        if path.startswith("/") or "\\" in path:
            raise SystemExit("candidate host archive path is unsafe")
        normalized = posixpath.normpath(path)
        if normalized in (".", "..") or normalized.startswith("../") or \
                normalized != path.rstrip("/"):
            raise SystemExit("candidate host archive path is unsafe")
        if not (member.isdir() or member.isreg()):
            raise SystemExit("candidate host archive contains a non-regular entry")
        roots.add(normalized.split("/", 1)[0])
        actual.add(path.rstrip("/"))
    if roots != {expected_root}:
        raise SystemExit("candidate host archive root is invalid")
    required = {
        expected_root + "/packaging/release/physical-e2e.sh",
    }
    if not required.issubset(actual):
        raise SystemExit("candidate host archive is missing release harnesses")
    os.makedirs(destination, mode=0o700, exist_ok=True)
    for member in members:
        bundle.extract(member, destination)
print(os.path.join(destination, expected_root))
PY
) || fail "cannot safely extract candidate release harnesses"
    [ -d "$CANDIDATE_HARNESS_ROOT" ] && [ ! -L "$CANDIDATE_HARNESS_ROOT" ] ||
        fail "candidate release harness root is unsafe"
    if [ -z "$GUEST_E2E" ]; then
        GUEST_E2E=$CANDIDATE_HARNESS_ROOT/packaging/release/physical-e2e.sh
    fi
fi
safe_harness "$GUEST_E2E" ||
    fail "physical E2E harness must name one owned absolute executable"

# Harnesses receive only exact candidate paths and isolated output paths; no
# source rebuild occurs during physical validation.
E2E_OUTPUT=$WORK/e2e.json
env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    HAMN_CANDIDATE_DIR="$CANDIDATE_DIR" \
    HAMN_CANDIDATE_JSON="$candidate" \
    HAMN_E2E_OUTPUT="$E2E_OUTPUT" \
    "$GUEST_E2E"
[ -s "$E2E_OUTPUT" ] && [ ! -L "$E2E_OUTPUT" ] ||
    fail "guest E2E harness did not produce evidence"

python3 - "$E2E_OUTPUT" "$CANDIDATE_HASH" \
    "$HOST_ARTIFACT_HASH" "$GUEST_IMAGE_HASH" "$SOURCE_TREE" \
    "$ALLOW_TEST_FIXTURES" <<'PY'
import json
import re
import sys

(e2e_path, candidate_hash, host_hash, guest_hash, source_tree,
 allow_test_fixtures) = sys.argv[1:]
with open(e2e_path, encoding="utf-8") as source:
    e2e = json.load(source)

def contains_fixture(value):
    if isinstance(value, str):
        return "fixture" in value.lower()
    if isinstance(value, dict):
        return any(contains_fixture(key) or contains_fixture(item)
                   for key, item in value.items())
    if isinstance(value, list):
        return any(contains_fixture(item) for item in value)
    return False

e2e_tests = {
    "lifecycle", "multiProfile", "staleSocket", "dockerContextRestore",
    "mount", "network", "tcpPort", "udpPort", "amd64", "rosetta", "dockerCli",
    "compose", "buildx", "dockerSdkGo", "dockerSdkPython",
    "testcontainersJava", "testcontainersGo", "testcontainersPython",
    "testcontainersNode", "k3s", "updateRollback", "softDeleteRecovery",
    "hardDelete",
    "uninstall", "colimaCoexistence",
}
if set(e2e) != {"schemaVersion", "kind", "passed", "tests", "provenance",
                "colima"} or \
        e2e["schemaVersion"] != 1 or e2e["kind"] != "hamn-physical-e2e" or \
        e2e["passed"] is not True or not isinstance(e2e["tests"], dict) or \
        set(e2e["tests"]) != e2e_tests or any(value is not True for value in e2e["tests"].values()) or \
        not isinstance(e2e["provenance"], dict) or \
        e2e["provenance"] != {"candidateJsonSha256": candidate_hash,
                              "hostArtifactSha256": host_hash,
                              "guestImageSha256": guest_hash}:
    raise SystemExit("physical E2E harness result is invalid")
colima = e2e["colima"]
if not isinstance(colima, dict) or \
        set(colima) != {"beforeSha256", "afterSha256", "binaryBeforeSha256",
                        "binaryAfterSha256", "instancesBeforeSha256",
                        "instancesAfterSha256"} or \
        not all(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value)
                for value in colima.values()) or \
        colima["beforeSha256"] != colima["afterSha256"] or \
        colima["binaryBeforeSha256"] != colima["binaryAfterSha256"] or \
        colima["instancesBeforeSha256"] != colima["instancesAfterSha256"]:
    raise SystemExit("Colima coexistence evidence is invalid or changed")
if allow_test_fixtures == "0" and contains_fixture(e2e):
    raise SystemExit("fixture-labelled evidence is forbidden outside tests")
PY

E2E_HASH=$(sha256_file "$E2E_OUTPUT")
E2E_HARNESS_HASH=$(sha256_file "$GUEST_E2E")
CHECKSUMS_HASH=$(sha256_file "$checksums")
EVIDENCE=$OUTPUT_DIR/validation-evidence.json
python3 - "$EVIDENCE" "$candidate" "$RELEASE_TAG" "$COMMIT" "$SOURCE_TREE" \
    local local "$VALIDATOR_IDENTITY" \
    "$CANDIDATE_HASH" "$CHECKSUMS_HASH" "$E2E_HASH" \
    "$E2E_HARNESS_HASH" <<'PY'
import json
import sys

(path, candidate_path, tag, commit, tree, run, attempt, identity,
 candidate_hash, checksums_hash, e2e_hash, e2e_harness_hash) = sys.argv[1:]
with open(candidate_path, encoding="utf-8") as source:
    candidate = json.load(source)
artifacts = {item["name"]: item["sha256"] for item in candidate["artifacts"]}
value = {
    "schemaVersion": 1,
    "kind": "hamn-validation-evidence",
    "tag": tag,
    "commit": commit,
    "sourceTree": tree,
    "workflow": {"run": run, "attempt": attempt},
    "validator": {
        "identity": identity,
        "harnesses": {
            "e2eSha256": e2e_harness_hash,
        },
    },
    "candidate": {"candidateJsonSha256": candidate_hash,
                  "checksumsSha256": checksums_hash,
                  "artifacts": artifacts},
    "tests": {"e2eSha256": e2e_hash},
}
with open(path, "w", encoding="utf-8", newline="\n") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
ssh-keygen -Y sign -f "$VALIDATOR_KEY" -n hamn-validator "$EVIDENCE" \
    >/dev/null
mv "$E2E_OUTPUT" "$OUTPUT_DIR/e2e.json"
chmod 0644 "$OUTPUT_DIR"/*.json "$OUTPUT_DIR"/*.sig
echo "validated exact candidate ${RELEASE_TAG}; evidence is in ${OUTPUT_DIR}"
