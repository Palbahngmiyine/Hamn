#!/bin/bash
# Recover an unpublished manifest version from the current protected main.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn release request: $*" >&2
    exit 1
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
OUTPUT=${GITHUB_OUTPUT:-}
COMMIT=${GITHUB_SHA:-}

[ "${GITHUB_EVENT_NAME:-}" = workflow_dispatch ] ||
    fail "only workflow_dispatch may recover an unpublished release"
[ "${GITHUB_REF:-}" = refs/heads/main ] ||
    fail "release recovery must run from main"
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || fail "GITHUB_SHA must be a full commit SHA"
[ "$COMMIT" = "$(git -C "$ROOT" rev-parse HEAD)" ] ||
    fail "GITHUB_SHA does not match the checked-out commit"
[ -n "$OUTPUT" ] && [ -f "$OUTPUT" ] && [ ! -L "$OUTPUT" ] ||
    fail "GITHUB_OUTPUT must name an existing regular file"
[[ "${GITHUB_RUN_ID:-}" =~ ^[1-9][0-9]*$ ]] ||
    fail "GITHUB_RUN_ID must be a positive decimal"

version=$(python3 - "$ROOT" <<'PY'
import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
with open(root / ".release-please-manifest.json", encoding="utf-8") as source:
    manifest = json.load(source)
if not isinstance(manifest, dict) or set(manifest) != {"."}:
    raise SystemExit("release manifest must contain only the root package")
version = manifest["."]
if not isinstance(version, str) or not re.fullmatch(
        r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
    raise SystemExit("release version is not canonical SemVer")
if (root / "version.txt").read_text(encoding="utf-8") != version + "\n":
    raise SystemExit("version.txt does not match the release manifest")
makefile = (root / "Makefile").read_text(encoding="utf-8")
make_versions = re.findall(
    r"^VERSION[ \t]+\?=[ \t]+([^ \t#\r\n]+)[ \t]*$", makefile, re.MULTILINE)
if make_versions != [version]:
    raise SystemExit("Makefile does not match the release manifest")
flake = (root / "flake.nix").read_text(encoding="utf-8")
flake_versions = re.findall(
    r'^\s*hamnVersion = "([^"]+)";\s*# x-release-please-version\s*$',
    flake, re.MULTILINE)
if flake_versions != [version]:
    raise SystemExit("flake.nix does not match the release manifest")
print(version)
PY
) || fail "cannot resolve the current release version"
stable_tag=v$version
if git -C "$ROOT" rev-parse --verify --quiet "refs/tags/$stable_tag" >/dev/null; then
    fail "stable tag already exists: $stable_tag"
fi
candidate_tag=$stable_tag-rc.${GITHUB_RUN_ID}
{
    printf 'should_release=true\n'
    printf 'version=%s\n' "$version"
    printf 'stable_tag=%s\n' "$stable_tag"
    printf 'candidate_tag=%s\n' "$candidate_tag"
    printf 'commit=%s\n' "$COMMIT"
} >>"$OUTPUT"
echo "recovered unpublished release $stable_tag from protected main"
