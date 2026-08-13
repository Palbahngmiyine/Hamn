#!/bin/bash
# Promote a validated RC without rebuilding any byte. GitHub release upload is
# intentionally a separate last step after this verifier succeeds.
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
VALIDATOR_KEY=${HAMN_VALIDATOR_PUBLIC_KEY:-}
RELEASE_KEY=${HAMN_RELEASE_SIGNING_KEY:-}
EXPECTED_WORKFLOW_RUN=${HAMN_EXPECTED_WORKFLOW_RUN:-}
EXPECTED_WORKFLOW_ATTEMPT=${HAMN_EXPECTED_WORKFLOW_ATTEMPT:-}
RELEASE_REPOSITORY=${HAMN_RELEASE_REPOSITORY:-}
RELEASE_BASE_URL=${HAMN_RELEASE_BASE_URL:-}

[ -n "$STABLE_TAG" ] && [ -n "$RC_TAG" ] && [ -n "$RELEASE_REF" ] &&
    [ -n "$INPUT_DIR" ] && [ -n "$OUTPUT_DIR" ] ||
    fail "usage: publish-release.sh vX.Y.Z vX.Y.Z-rc.N COMMIT INPUT_DIR OUTPUT_DIR"
[[ "$STABLE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail "stable tag is invalid"
[[ "$RC_TAG" =~ ^${STABLE_TAG}-rc\.[0-9]+$ ]] ||
    fail "RC tag does not correspond to the stable tag"
[[ "$EXPECTED_WORKFLOW_RUN" =~ ^[1-9][0-9]*$ ]] ||
    fail "HAMN_EXPECTED_WORKFLOW_RUN must be a positive decimal run ID"
[[ "$EXPECTED_WORKFLOW_ATTEMPT" =~ ^[1-9][0-9]*$ ]] ||
    fail "HAMN_EXPECTED_WORKFLOW_ATTEMPT must be a positive decimal attempt"
if [ -n "$RELEASE_REPOSITORY" ]; then
    [[ "$RELEASE_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
        fail "HAMN_RELEASE_REPOSITORY is invalid"
    [ -z "$RELEASE_BASE_URL" ] ||
        fail "HAMN_RELEASE_BASE_URL must not override the canonical GitHub Release base"
    BASE_URL="https://github.com/${RELEASE_REPOSITORY}/releases/download/${STABLE_TAG}"
else
    BASE_URL=$RELEASE_BASE_URL
    [ -n "$BASE_URL" ] || fail "HAMN_RELEASE_BASE_URL is required outside GitHub Actions"
fi
case "$BASE_URL" in https://*) ;; *) fail "release base URL must use HTTPS" ;; esac
CANONICAL_MANIFEST_URL=$BASE_URL/hamn-update-manifest.json
[ -d "$INPUT_DIR" ] && [ ! -L "$INPUT_DIR" ] || fail "INPUT_DIR is unsafe"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] || fail "OUTPUT_DIR is unsafe"
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] ||
    fail "OUTPUT_DIR must be empty"
safe_regular "$VALIDATOR_KEY" || fail "HAMN_VALIDATOR_PUBLIC_KEY is unavailable"
ssh-keygen -lf "$VALIDATOR_KEY" | grep -q 'ED25519' ||
    fail "validator public key is not Ed25519"
safe_regular "$RELEASE_KEY" || fail "HAMN_RELEASE_SIGNING_KEY is unavailable"
RELEASE_PUBLIC=$(ssh-keygen -y -f "$RELEASE_KEY") ||
    fail "release signing key is not an Ed25519 private key"
RELEASE_PUBLIC=$(printf '%s\n' "$RELEASE_PUBLIC" | awk '
    (NF == 2 || NF == 3) && $1 == "ssh-ed25519" && $2 != "" {
        print $1 " " $2
        exit
    }
')
[ -n "$RELEASE_PUBLIC" ] || fail "release signing key is not Ed25519"
VALIDATOR_PUBLIC=$(awk '
    (NF == 2 || NF == 3) && $1 == "ssh-ed25519" && $2 != "" {
        print $1 " " $2
        exit
    }
' "$VALIDATOR_KEY")
[ -n "$VALIDATOR_PUBLIC" ] || fail "validator public key is malformed"
[ "$RELEASE_PUBLIC" != "$VALIDATOR_PUBLIC" ] ||
    fail "release and validator keys must be distinct"

CANDIDATE_DIR=$INPUT_DIR/hamn-candidate
EVIDENCE_DIR=$INPUT_DIR/hamn-evidence
[ -d "$CANDIDATE_DIR" ] && [ -d "$EVIDENCE_DIR" ] ||
    fail "expected candidate and evidence artifact directories are missing"
version=${STABLE_TAG#v}
HOST_FILE="hamn-${STABLE_TAG}-darwin-arm64.tar.gz"
GUEST_FILE="hamn-${STABLE_TAG}-ubuntu-24.04-arm64.img"
SBOM_FILE="hamn-${STABLE_TAG}.spdx.json"
INSTALLER_FILE=install.sh
candidate=$CANDIDATE_DIR/candidate.json
checksums=$CANDIDATE_DIR/SHA256SUMS
evidence=$EVIDENCE_DIR/validation-evidence.json
signature=$EVIDENCE_DIR/validation-evidence.json.sig
python3 - "$CANDIDATE_DIR" "$HOST_FILE" "$GUEST_FILE" "$SBOM_FILE" \
    "$INSTALLER_FILE" <<'PY'
import os
import stat
import sys

directory, host, guest, sbom, installer = sys.argv[1:]
expected = {host, guest, sbom, installer, "candidate.json", "SHA256SUMS"}
actual = set()
with os.scandir(directory) as entries:
    for entry in entries:
        info = entry.stat(follow_symlinks=False)
        if entry.is_symlink() or not stat.S_ISREG(info.st_mode):
            raise SystemExit("candidate artifact directory contains an unsafe entry")
        actual.add(entry.name)
if actual != expected:
    raise SystemExit("candidate artifact directory contains unexpected entries")
PY
for file in "$candidate" "$checksums" "$CANDIDATE_DIR/$HOST_FILE" \
    "$CANDIDATE_DIR/$GUEST_FILE" "$CANDIDATE_DIR/$SBOM_FILE" \
    "$CANDIDATE_DIR/$INSTALLER_FILE" "$evidence" "$signature"; do
    safe_regular "$file" || fail "unsafe release input: $file"
done
(cd "$CANDIDATE_DIR" && shasum -a 256 -c SHA256SUMS) ||
    fail "candidate artifact hashes do not match"

allowed=$(mktemp "${TMPDIR:-/tmp}/hamn-validator.XXXXXX") ||
    fail "cannot create allowed signers file"
cleanup() {
    rm -f "$allowed"
}
trap cleanup EXIT
printf 'hamn-validator ' >"$allowed"
cat "$VALIDATOR_KEY" >>"$allowed"
ssh-keygen -Y verify -f "$allowed" -I hamn-validator -n hamn-validator \
    -s "$signature" <"$evidence" >/dev/null ||
    fail "validator evidence signature verification failed"

COMMIT=$(git -C "$ROOT" rev-parse --verify "$RELEASE_REF^{commit}") ||
    fail "RELEASE_REF is not a commit"
SOURCE_TREE=$(git -C "$ROOT" rev-parse "$COMMIT^{tree}") ||
    fail "cannot resolve release source tree"
CANDIDATE_HASH=$(sha256_file "$candidate")
CHECKSUMS_HASH=$(sha256_file "$checksums")
E2E=$EVIDENCE_DIR/e2e.json
safe_regular "$E2E" || fail "E2E evidence is unavailable"
E2E_HASH=$(sha256_file "$E2E")

python3 - "$candidate" "$checksums" "$evidence" "$STABLE_TAG" "$RC_TAG" "$COMMIT" "$SOURCE_TREE" \
    "$CANDIDATE_HASH" "$CHECKSUMS_HASH" "$E2E_HASH" "$EXPECTED_WORKFLOW_RUN" \
    "$EXPECTED_WORKFLOW_ATTEMPT" <<'PY'
import json
import re
import sys

(candidate_path, checksums_path, evidence_path, stable_tag, tag, commit, tree,
 candidate_hash, checksums_hash, e2e_hash, expected_run, expected_attempt) = sys.argv[1:]
with open(candidate_path, encoding="utf-8") as source:
    candidate = json.load(source)
with open(checksums_path, encoding="utf-8") as source:
    checksum_lines = source.readlines()
with open(evidence_path, encoding="utf-8") as source:
    evidence = json.load(source)
if candidate.get("tag") != tag or candidate.get("commit") != commit or \
        candidate.get("sourceTree") != tree or candidate.get("version") != stable_tag:
    raise SystemExit("candidate provenance mismatch")
artifacts = candidate.get("artifacts")
if not isinstance(artifacts, list):
    raise SystemExit("candidate artifacts are invalid")
expected_names = {
    "hamn-" + stable_tag + "-darwin-arm64.tar.gz",
    "hamn-" + stable_tag + "-ubuntu-24.04-arm64.img",
    "hamn-" + stable_tag + ".spdx.json",
    "install.sh",
}
by_name = {}
for artifact in artifacts:
    if not isinstance(artifact, dict) or set(artifact) != {"name", "sha256"} or \
            not isinstance(artifact["name"], str) or \
            not re.fullmatch(r"[A-Za-z0-9._-]+", artifact["name"]) or \
            not isinstance(artifact["sha256"], str) or \
            not re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"]) or \
            artifact["name"] in by_name:
        raise SystemExit("candidate artifact is invalid")
    by_name[artifact["name"]] = artifact["sha256"]
if set(by_name) != expected_names:
    raise SystemExit("candidate artifact names are invalid")
checksum_entries = {}
for line in checksum_lines:
    match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9._-]+)\n", line)
    if not match or match.group(2) in checksum_entries:
        raise SystemExit("SHA256SUMS is invalid")
    checksum_entries[match.group(2)] = match.group(1)
if set(checksum_entries) != expected_names | {"candidate.json"} or \
        any(checksum_entries[name] != digest for name, digest in by_name.items()):
    raise SystemExit("candidate metadata does not match SHA256SUMS")
if set(evidence) != {"schemaVersion", "kind", "tag", "commit", "sourceTree",
                     "workflow", "validator", "candidate", "tests"} or \
        evidence.get("schemaVersion") != 1 or \
        evidence.get("kind") != "hamn-validation-evidence" or \
        evidence.get("tag") != tag or evidence.get("commit") != commit or \
        evidence.get("sourceTree") != tree:
    raise SystemExit("validation provenance mismatch")
workflow = evidence["workflow"]
validator = evidence["validator"]
if not isinstance(workflow, dict) or set(workflow) != {"run", "attempt"} or \
        workflow != {"run": expected_run, "attempt": expected_attempt}:
    raise SystemExit("validation workflow provenance does not match verified RC run and attempt")
if not isinstance(validator, dict) or set(validator) != {"identity", "harnesses"} or \
        not isinstance(validator["identity"], str) or not validator["identity"] or \
        not isinstance(validator["harnesses"], dict) or \
        set(validator["harnesses"]) != {"e2eSha256"} or \
        not all(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value)
                for value in validator["harnesses"].values()):
    raise SystemExit("validation harness provenance is invalid")
if evidence.get("candidate") != {"candidateJsonSha256": candidate_hash,
                                 "checksumsSha256": checksums_hash,
                                 "artifacts": by_name}:
    raise SystemExit("validation did not cover these exact candidate bytes")
if evidence.get("tests") != {"e2eSha256": e2e_hash}:
    raise SystemExit("validation evidence payload changed")
PY

EMBEDDED_RELEASE_PUBLIC=$(tar -xOf "$CANDIDATE_DIR/$HOST_FILE" \
    "hamn-${STABLE_TAG}-darwin-arm64/packaging/release/hamn-release.pub" 2>/dev/null | \
    awk '
        (NF == 2 || NF == 3) && $1 == "ssh-ed25519" && $2 != "" {
            print $1 " " $2
            exit
        }
    ') || fail "candidate release public key is unavailable"
[ -n "$EMBEDDED_RELEASE_PUBLIC" ] ||
    fail "candidate release public key is malformed"
[ "$EMBEDDED_RELEASE_PUBLIC" = "$RELEASE_PUBLIC" ] ||
    fail "candidate installer release key does not match release signing key"
EMBEDDED_MANIFEST_URL=$(tar -xOf "$CANDIDATE_DIR/$HOST_FILE" \
    "hamn-${STABLE_TAG}-darwin-arm64/packaging/release/update-manifest-url" 2>/dev/null) ||
    fail "candidate update manifest URL is unavailable"
[ "$EMBEDDED_MANIFEST_URL" = "$CANONICAL_MANIFEST_URL" ] ||
    fail "candidate update manifest URL does not match the canonical stable release"
HOST_HASH=$(sha256_file "$CANDIDATE_DIR/$HOST_FILE")
GUEST_HASH=$(sha256_file "$CANDIDATE_DIR/$GUEST_FILE")

# Canonical URLs point at same bytes uploaded from CANDIDATE_DIR by the
# workflow; no compiler or image builder runs in this promotion step.
MANIFEST=$OUTPUT_DIR/hamn-update-manifest.json
python3 - "$MANIFEST" "$version" "$BASE_URL" "$HOST_FILE" "$HOST_HASH" \
    "$GUEST_FILE" "$GUEST_HASH" <<'PY'
import json
import sys

path, version, base, host_name, host_hash, guest_name, guest_hash = sys.argv[1:]
value = {
    "schemaVersion": 1,
    "channel": "stable",
    "version": "v" + version,
    "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
    "artifacts": {
        "host": {"url": base.rstrip("/") + "/" + host_name, "sha256": host_hash},
        "guestImage": {"url": base.rstrip("/") + "/" + guest_name, "sha256": guest_hash},
    },
}
with open(path, "w", encoding="utf-8", newline="\n") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
ssh-keygen -Y sign -f "$RELEASE_KEY" -n hamn-release "$MANIFEST" >/dev/null
cp "$candidate" "$OUTPUT_DIR/candidate.json"
cp "$checksums" "$OUTPUT_DIR/SHA256SUMS"
cp "$evidence" "$OUTPUT_DIR/validation-evidence.json"
cp "$signature" "$OUTPUT_DIR/validation-evidence.json.sig"
cp "$E2E" "$OUTPUT_DIR/e2e.json"
printf '%s\n' "$RC_TAG" >"$OUTPUT_DIR/promoted-from-rc"
chmod 0644 "$OUTPUT_DIR"/*
echo "verified exact RC ${RC_TAG}; publish candidate bytes and ${MANIFEST##*/} from ${OUTPUT_DIR}"
