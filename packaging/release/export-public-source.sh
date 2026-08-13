#!/bin/bash
# Create a local, single-root public-source repository from one exact Hamn
# commit. This deliberately never changes a remote, renames a repository, or
# deletes private history; those externally visible steps require an explicit
# operator decision after the export has been inspected.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn public export: $*" >&2
    exit 1
}

usage() {
    cat >&2 <<'EOF'
usage: export-public-source.sh OUTPUT_DIRECTORY

Create OUTPUT_DIRECTORY as a new Git repository with exactly one root commit
whose tree is identical to the checked-out Hamn source commit. The destination
must not exist. No remote is added or changed.
EOF
    exit 2
}

[ "$#" = 1 ] || usage

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
OUTPUT=$1
case "$OUTPUT" in
/*) ;;
*) fail "output directory must be absolute" ;;
esac
[ ! -e "$OUTPUT" ] && [ ! -L "$OUTPUT" ] ||
    fail "output directory already exists"

PARENT=$(dirname "$OUTPUT")
[ -d "$PARENT" ] && [ ! -L "$PARENT" ] ||
    fail "output parent directory is unsafe"

COMMIT=$(git -C "$ROOT" rev-parse --verify HEAD^{commit}) ||
    fail "cannot resolve checked-out commit"
TREE=$(git -C "$ROOT" rev-parse "$COMMIT^{tree}") ||
    fail "cannot resolve checked-out source tree"

# Desktop is intentionally untracked/user-owned in the CLI-only repository.
# git archive excludes all uncommitted content and includes only the exact
# commit tree, but make the tracked Desktop boundary loud.
if git -C "$ROOT" ls-tree -r --name-only "$COMMIT" -- desktop | grep -q .; then
    fail "source commit still contains tracked desktop assets"
fi

mkdir -m 0755 "$OUTPUT"
git -C "$ROOT" archive --format=tar "$COMMIT" | tar -x -C "$OUTPUT" ||
    fail "cannot extract tracked source tree"

git -C "$OUTPUT" init -q --initial-branch main
git -C "$OUTPUT" add -A
GIT_AUTHOR_NAME='Hamn Release Export' \
GIT_AUTHOR_EMAIL='release-export@invalid' \
GIT_COMMITTER_NAME='Hamn Release Export' \
GIT_COMMITTER_EMAIL='release-export@invalid' \
    git -C "$OUTPUT" commit -qm 'Initial Hamn 0.0.1 source'

[ "$(git -C "$OUTPUT" rev-list --all --count)" = 1 ] ||
    fail "public export does not have exactly one root commit"
[ "$(git -C "$OUTPUT" rev-parse HEAD^{tree})" = "$TREE" ] ||
    fail "public export tree does not match the source commit"
git -C "$OUTPUT" fsck --no-reflogs >/dev/null ||
    fail "public export repository is invalid"

printf 'exported %s as one root commit at %s\n' "$COMMIT" "$OUTPUT"
