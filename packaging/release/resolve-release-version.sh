#!/bin/bash
# Resolve a Release Please manifest transition into one automated release run.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn automated release: $*" >&2
    exit 1
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
PREVIOUS_REF=${1:-}
OUTPUT=${GITHUB_OUTPUT:-}
RUN_ID=${GITHUB_RUN_ID:-}
COMMIT=${GITHUB_SHA:-}

[[ "$PREVIOUS_REF" =~ ^[0-9a-f]{40}$ ]] ||
    fail "previous release ref must be a full commit SHA"
[ -n "$OUTPUT" ] && [ -f "$OUTPUT" ] && [ ! -L "$OUTPUT" ] ||
    fail "GITHUB_OUTPUT must name an existing regular file"

if [ "$PREVIOUS_REF" = 0000000000000000000000000000000000000000 ] ||
        ! git -C "$ROOT" cat-file -e \
            "$PREVIOUS_REF:.release-please-manifest.json" 2>/dev/null; then
    printf 'should_release=false\n' >>"$OUTPUT"
    echo "Release Please bootstrap detected; no release is due"
    exit 0
fi

[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] ||
    fail "GITHUB_SHA must be a full commit SHA"
[[ "$RUN_ID" =~ ^[1-9][0-9]*$ ]] ||
    fail "GITHUB_RUN_ID must be a positive decimal integer"
[ "$COMMIT" = "$(git -C "$ROOT" rev-parse --verify HEAD)" ] ||
    fail "GITHUB_SHA does not match the checked-out commit"

python3 - "$ROOT" "$PREVIOUS_REF" "$COMMIT" "$RUN_ID" "$OUTPUT" <<'PY'
import json
import pathlib
import re
import subprocess
import sys

root, previous_ref, commit, run_id, output_path = sys.argv[1:]


def blob(ref, path):
    return subprocess.check_output(
        ["git", "-C", root, "show", ref + ":" + path], text=True
    )


def manifest(ref):
    try:
        value = json.loads(blob(ref, ".release-please-manifest.json"))
    except (json.JSONDecodeError, subprocess.CalledProcessError) as error:
        raise SystemExit("release manifest is invalid: " + str(error))
    if not isinstance(value, dict) or set(value) != {"."}:
        raise SystemExit("release manifest must contain only the root package")
    return version(value["."])


def version(value):
    if not isinstance(value, str):
        raise SystemExit("release version is not a string")
    match = re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value)
    if not match:
        raise SystemExit("release version is not canonical SemVer")
    return value, tuple(int(part) for part in match.groups())


old_text, old_value = manifest(previous_ref)
new_text, new_value = manifest(commit)
if new_value <= old_value:
    raise SystemExit("release version did not increase")

version_file = blob(commit, "version.txt")
if version_file != new_text + "\n":
    raise SystemExit("version.txt does not match the release manifest")

makefile = blob(commit, "Makefile")
make_match = re.search(r"^VERSION[ \t]+\?=[ \t]+([^ \t#\r\n]+)[ \t]*$", makefile,
                       re.MULTILINE)
if not make_match or make_match.group(1) != new_text:
    raise SystemExit("Makefile does not match the release manifest")

flake = blob(commit, "flake.nix")
flake_versions = re.findall(
    r'^\s*hamnVersion = "([^"]+)";\s*# x-release-please-version\s*$',
    flake,
    re.MULTILINE,
)
if flake_versions != [new_text]:
    raise SystemExit("flake.nix does not match the release manifest")

stable_tag = "v" + new_text
candidate_tag = stable_tag + "-rc." + run_id
with open(output_path, "a", encoding="utf-8", newline="\n") as output:
    output.write("should_release=true\n")
    output.write("version=" + new_text + "\n")
    output.write("stable_tag=" + stable_tag + "\n")
    output.write("candidate_tag=" + candidate_tag + "\n")
    output.write("commit=" + commit + "\n")
PY

echo "automated release version resolved from Release Please manifest"
