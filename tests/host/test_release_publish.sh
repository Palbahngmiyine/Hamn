#!/bin/bash
# Keyless promotion verifies hosted evidence and exact candidate bytes. It
# creates immutable-release metadata without rebuilding or private keys.
set -euo pipefail
unset GITHUB_ACTIONS GITHUB_REPOSITORY GITHUB_RUN_ID GITHUB_RUN_ATTEMPT

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-publish.XXXXXX)
cleanup() {
    rm -rf "$WORK"
    make -C "$ROOT" host VERSION=0.0.1-dev >/dev/null
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
    bash "$ROOT/packaging/release/build-candidate.sh" >/dev/null

RELEASE_REF="$release_ref" \
RELEASE_TAG=v0.0.1-rc.417123456 \
CANDIDATE_DIR="$candidate" \
OUTPUT_DIR="$evidence" \
GITHUB_RUN_ID="$workflow_run" \
GITHUB_RUN_ATTEMPT="$workflow_attempt" \
    bash "$ROOT/packaging/release/hosted-validation.sh" >/dev/null

host=$candidate/hamn-v0.0.1-darwin-arm64.tar.gz
host_hash=$(sha256 "$host")
# Synthetic fixture evidence exercises the gate; it is never production image
# measurement and is only accepted through the explicit local-test boundary.
python3 - "$candidate/hamn-v0.0.1-ubuntu-24.04-arm64.img" \
    "$evidence/guest-image-size-report.json" "$WORK/size-budget.json" "$release_ref" <<'PY_SIZE'
import hashlib, json, pathlib, sys
image, report_path, budget_path, revision = sys.argv[1:]
data = pathlib.Path(image).read_bytes()
digest = hashlib.sha256(data).hexdigest()
baseline = len(data) + 64 * 1024**2
report = {"schemaVersion":1,"reviewOnly":False,"compressedBytes":len(data),"imageSha256":digest,
    "baselineCompressedBytes":baseline,"baselineSha256":"1"*64,"savedBytes":64*1024**2,
    "requiredSavingsBytes":64*1024**2,"virtualBytes":8*1024**3,"baseImageSha256":"2"*64,
    "sourceRevision":revision,"packagesBefore":["fixture"],"packagesAfter":["fixture"],"cleanup":["fixture only"],"runtimeValidation":"not executed"}
pathlib.Path(report_path).write_text(json.dumps(report))
budget={"schemaVersion":1,"maximumCompressedBytes":len(data)+1,"referenceImageSha256":digest,
    "footprintReportSha256":hashlib.sha256(pathlib.Path(report_path).read_bytes()).hexdigest()}
pathlib.Path(budget_path).write_text(json.dumps(budget))
PY_SIZE
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
# Keep publisher URLs and bytes unchanged; native curl uses a bounded local TLS
# CONNECT fixture with child-only trust, not a PATH replacement.
python3 "$ROOT/tests/host/publisher_consumer.py" "$publish" "$candidate" "$WORK/consumers"
python3 - "$publish/hamn-update-manifest.json" "$release_ref" \
    "$publish/hosted-validation-evidence.json" "$source_tree" \
    "$publish/hamn-update-manifest-v3.json" "$candidate" <<'PY'
import json
import os
import sys

manifest_path, commit, evidence_path, tree, v3_path, candidate = sys.argv[1:]
with open(manifest_path, encoding="utf-8") as source:
    manifest = json.load(source)
if manifest.get("schemaVersion") != 2 or manifest.get("version") != "v0.0.1" or \
        set(manifest) != {"schemaVersion", "channel", "version", "commit",
                         "validationMode", "compatibility", "artifacts"} or \
        manifest.get("commit") != commit or \
        manifest.get("validationMode") != "github-hosted-no-vm":
    raise SystemExit("keyless update manifest identity is invalid")
if not manifest["artifacts"]["host"]["url"].startswith(
        "https://github.com/example/hamn/releases/download/v0.0.1/"):
    raise SystemExit("host URL is not canonical")
with open(evidence_path, encoding="utf-8") as source:
    evidence = json.load(source)
if evidence.get("physicalE2E") is not False or \
        evidence.get("sourceTree") != tree:
    raise SystemExit("hosted evidence overstates validation")
with open(v3_path, encoding="utf-8") as source:
    v3 = json.load(source)
assert v3["schemaVersion"] == 3
for name in ("host", "guestImage"):
    old, new = manifest["artifacts"][name], v3["artifacts"][name]
    assert old["sha256"] == new["sha256"] and old["url"] == new["url"]
    assert new["size"] == os.path.getsize(os.path.join(candidate, new["url"].rsplit("/", 1)[1]))
PY

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
        python3 - "$evidence/guest-image-size-report.json" <<'PY_REVIEW'
import json, pathlib, sys
path=pathlib.Path(sys.argv[1]); value=json.loads(path.read_text())
value['reviewOnly']=True; path.write_text(json.dumps(value))
PY_REVIEW
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
    [ ! -e "$rejected/hamn-update-manifest.json" ]
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
python3 - "$evidence/hosted-validation-evidence.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
value["physicalE2E"] = True
with open(path, "w", encoding="utf-8", newline="\n") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
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
