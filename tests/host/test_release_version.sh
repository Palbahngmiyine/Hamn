#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-version.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

validate_release_state() {
    python3 - "$1" <<'PY'
import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
semver = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def read_text(relative):
    path = root / relative
    if not path.is_file() or path.is_symlink():
        raise SystemExit(f"release version source is missing or unsafe: {relative}")
    return path.read_text(encoding="utf-8")


def read_json(relative):
    try:
        return json.loads(read_text(relative))
    except json.JSONDecodeError as error:
        raise SystemExit(f"release JSON is invalid: {relative}: {error}")


config = read_json("release-please-config.json")
if not isinstance(config, dict):
    raise SystemExit("Release Please configuration must be an object")
initial = config.get("initial-version")
if initial != "0.0.1" or not semver.fullmatch(initial):
    raise SystemExit("initial release version policy is not 0.0.1")
if config.get("bump-minor-pre-major") is not True or \
        config.get("bump-patch-for-minor-pre-major") is not True:
    raise SystemExit("pre-major release policy is incomplete")

manifest = read_json(".release-please-manifest.json")
if not isinstance(manifest, dict) or set(manifest) != {"."}:
    raise SystemExit("release manifest must contain only the root package")
manifest_version = manifest["."]
if not isinstance(manifest_version, str) or not semver.fullmatch(manifest_version):
    raise SystemExit("manifest version is not canonical SemVer")

version_text = read_text("version.txt")
version_match = re.fullmatch(
    r"((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))\n",
    version_text,
)
if not version_match:
    raise SystemExit("version.txt is not one canonical SemVer line")
source_version = version_match.group(1)

make_versions = re.findall(
    r"^VERSION[ \t]+\?=[ \t]+([^ \t#\r\n]+)[ \t]*$",
    read_text("Makefile"),
    re.MULTILINE,
)
if make_versions != [source_version]:
    raise SystemExit("Makefile version does not match version.txt")

flake_versions = re.findall(
    r'^\s*hamnVersion = "([^"]+)";\s*# x-release-please-version\s*$',
    read_text("flake.nix"),
    re.MULTILINE,
)
if flake_versions != [source_version]:
    raise SystemExit("flake.nix version does not match version.txt")

if manifest_version == "0.0.0":
    if source_version != initial:
        raise SystemExit("bootstrap source version does not match initial-version")
elif source_version != manifest_version:
    raise SystemExit("released source version does not match the manifest")
PY
}

write_state_fixture() {
    local manifest=$1 source=$2 make_version=$3 flake_version=$4
    local bump_minor=${5:-true}
    local initial=${6:-0.0.1}
    local state=$WORK/state

    mkdir -p "$state"
    printf '%s\n' \
        "{\"initial-version\":\"$initial\",\"bump-minor-pre-major\":$bump_minor,\"bump-patch-for-minor-pre-major\":true}" \
        >"$state/release-please-config.json"
    printf '{".":"%s"}\n' "$manifest" >"$state/.release-please-manifest.json"
    printf '%s\n' "$source" >"$state/version.txt"
    printf '# x-release-please-start-version\nVERSION    ?= %s\n# x-release-please-end\n' \
        "$make_version" >"$state/Makefile"
    printf '      hamnVersion = "%s"; # x-release-please-version\n' \
        "$flake_version" >"$state/flake.nix"
}

expect_state_failure() {
    local expected=$1
    local state=$WORK/state

    if validate_release_state "$state" >"$WORK/state.out" 2>"$WORK/state.err"; then
        fail "release state validation accepted: $expected"
    fi
    grep -Fq "$expected" "$WORK/state.err" ||
        fail "release state validation did not report: $expected"
}

validate_release_state "$ROOT"

write_state_fixture 0.0.0 0.0.1 0.0.1 0.0.1
validate_release_state "$WORK/state"
write_state_fixture 0.0.1 0.0.1 0.0.1 0.0.1
validate_release_state "$WORK/state"
printf '%s\n' '[]' >"$WORK/state/release-please-config.json"
expect_state_failure "Release Please configuration must be an object"
write_state_fixture 0.0.1 0.0.1 0.0.1 0.0.1 true 0.0.2
expect_state_failure "initial release version policy is not 0.0.1"
write_state_fixture 0.0.1 0.0.1 0.0.1 0.0.1
printf '%s\n' '{".":"0.0.1","guest":"0.0.1"}' \
    >"$WORK/state/.release-please-manifest.json"
expect_state_failure "release manifest must contain only the root package"
write_state_fixture 01.0.0 0.0.1 0.0.1 0.0.1
expect_state_failure "manifest version is not canonical SemVer"
write_state_fixture 0.0.1 01.0.0 01.0.0 01.0.0
expect_state_failure "version.txt is not one canonical SemVer line"
write_state_fixture 0.0.1 0.0.1 0.0.1 0.0.1
printf '0.0.1\n0.0.2\n' >"$WORK/state/version.txt"
expect_state_failure "version.txt is not one canonical SemVer line"
write_state_fixture 0.0.0 0.0.2 0.0.2 0.0.2
expect_state_failure "bootstrap source version does not match initial-version"
write_state_fixture 0.0.1 0.0.2 0.0.2 0.0.2
expect_state_failure "released source version does not match the manifest"
write_state_fixture 0.0.1 0.0.1 0.0.2 0.0.1
expect_state_failure "Makefile version does not match version.txt"
write_state_fixture 0.0.1 0.0.1 0.0.1 0.0.2
expect_state_failure "flake.nix version does not match version.txt"
write_state_fixture 0.0.1 0.0.1 0.0.1 0.0.1 false
expect_state_failure "pre-major release policy is incomplete"

REPO=$WORK/repository
mkdir -p "$REPO/packaging/release"
cp "$ROOT/packaging/release/resolve-release-version.sh" \
    "$REPO/packaging/release/resolve-release-version.sh"
git -C "$REPO" init -q
git -C "$REPO" config user.name 'Hamn Test'
git -C "$REPO" config user.email 'hamn-test@invalid'

write_sources() {
    local version=$1
    printf '%s\n' "$version" >"$REPO/version.txt"
    printf '# x-release-please-start-version\nVERSION    ?= %s\n# x-release-please-end\n' \
        "$version" >"$REPO/Makefile"
    printf '      hamnVersion = "%s"; # x-release-please-version\n' "$version" \
        >"$REPO/flake.nix"
}

commit_all() {
    git -C "$REPO" add .
    git -C "$REPO" commit -q -m "$1"
    git -C "$REPO" rev-parse HEAD
}

run_resolver() {
    local previous=$1 output=$2
    : >"$output"
    GITHUB_OUTPUT="$output" \
    GITHUB_RUN_ID=417123456 \
    GITHUB_SHA=$(git -C "$REPO" rev-parse HEAD) \
        bash "$REPO/packaging/release/resolve-release-version.sh" "$previous"
}

write_sources 0.0.1
BASE=$(commit_all 'base without Release Please')
printf '%s\n' '{".":"0.0.0"}' >"$REPO/.release-please-manifest.json"
BOOTSTRAP=$(commit_all 'bootstrap Release Please')
run_resolver "$BASE" "$WORK/bootstrap.output"
grep -Fxq 'should_release=false' "$WORK/bootstrap.output"

printf '%s\n' '{".":"0.0.1"}' >"$REPO/.release-please-manifest.json"
RELEASE=$(commit_all 'release 0.0.1')
run_resolver "$BOOTSTRAP" "$WORK/release.output"
for expected in \
    'should_release=true' \
    'version=0.0.1' \
    'stable_tag=v0.0.1' \
    'candidate_tag=v0.0.1-rc.417123456' \
    "commit=$RELEASE"; do
    grep -Fxq "$expected" "$WORK/release.output"
done

write_sources 0.0.2
printf '%s\n' '{".":"0.0.1"}' >"$REPO/.release-please-manifest.json"
MISMATCH=$(commit_all 'mismatched source version')
if run_resolver "$BOOTSTRAP" "$WORK/mismatch.output" \
    >"$WORK/mismatch.out" 2>"$WORK/mismatch.err"; then
    echo "FAIL: release resolver accepted mismatched source versions" >&2
    exit 1
fi
grep -Fq 'version.txt does not match the release manifest' "$WORK/mismatch.err"

write_sources 0.0.1
printf '%s\n' '{".":"0.0.0"}' >"$REPO/.release-please-manifest.json"
ROLLBACK=$(commit_all 'non-increasing release')
if run_resolver "$BOOTSTRAP" "$WORK/rollback.output" \
    >"$WORK/rollback.out" 2>"$WORK/rollback.err"; then
    echo "FAIL: release resolver accepted a non-increasing version" >&2
    exit 1
fi
grep -Fq 'release version did not increase' "$WORK/rollback.err"

: >"$WORK/missing-run.output"
if GITHUB_OUTPUT="$WORK/missing-run.output" GITHUB_RUN_ID= \
GITHUB_SHA="$RELEASE" \
    bash "$REPO/packaging/release/resolve-release-version.sh" "$BOOTSTRAP" \
    >"$WORK/missing-run.out" 2>"$WORK/missing-run.err"; then
    echo "FAIL: release resolver accepted a missing workflow run ID" >&2
    exit 1
fi
grep -Fq 'GITHUB_RUN_ID must be a positive decimal integer' \
    "$WORK/missing-run.err"

echo "PASS: Release Please manifest transitions gate automated releases"
