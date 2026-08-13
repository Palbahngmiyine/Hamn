#!/bin/bash
# The public export must contain only one root commit from the exact tracked
# source tree. It must never absorb the user-owned untracked desktop/ tree.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-public-export.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

OUTPUT=$WORK/public-hamn
"$ROOT/packaging/release/export-public-source.sh" "$OUTPUT" \
    >"$WORK/export.out"

[ "$(git -C "$OUTPUT" rev-list --all --count)" = 1 ] || {
    echo "FAIL: public export has more than one commit" >&2
    exit 1
}
[ "$(git -C "$OUTPUT" rev-parse HEAD^{tree})" = \
  "$(git -C "$ROOT" rev-parse HEAD^{tree})" ] || {
    echo "FAIL: public export tree differs from the exact source tree" >&2
    exit 1
}
[ ! -e "$OUTPUT/desktop" ] || {
    echo "FAIL: public export included untracked desktop assets" >&2
    exit 1
}
git -C "$OUTPUT" remote | grep -q . && {
    echo "FAIL: public export unexpectedly configured a remote" >&2
    exit 1
}
git -C "$OUTPUT" fsck --no-reflogs >/dev/null

if "$ROOT/packaging/release/export-public-source.sh" "$OUTPUT" \
    >"$WORK/reused.out" 2>"$WORK/reused.err"; then
    echo "FAIL: public export overwrote an existing destination" >&2
    exit 1
fi
grep -Fq 'output directory already exists' "$WORK/reused.err"

echo "PASS: public source export is single-root and remote-free"
