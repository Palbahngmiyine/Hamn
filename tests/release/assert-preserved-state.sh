#!/bin/bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 EXPECTED_MANIFEST STATE_ROOT" >&2
    exit 2
fi

EXPECTED=$1
ROOT=$2
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

[ -f "$EXPECTED" ] || {
    echo "FAIL: expected preservation manifest does not exist: $EXPECTED" >&2
    exit 1
}
[ -d "$ROOT" ] || {
    echo "FAIL: preserved state root was removed: $ROOT" >&2
    exit 1
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
actual="$work/actual.manifest"
"$SCRIPT_DIR/snapshot-preserved-state.sh" "$ROOT" "$actual" >/dev/null

if ! cmp -s "$EXPECTED" "$actual"; then
    echo "FAIL: preserved Hamn state changed" >&2
    diff -u "$EXPECTED" "$actual" >&2 || true
    exit 1
fi

echo "OK: Hamn state is unchanged"
