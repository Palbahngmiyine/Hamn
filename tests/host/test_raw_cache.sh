#!/bin/bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/hamn-raw-cache-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
FLAGS=(-std=c11 -Wall -Wextra -Werror=implicit-function-declaration -Wno-deprecated-declarations -DHAMN_TEST -I"$ROOT/host")
if [ "${HAMN_TEST_SANITIZERS:-0}" = 1 ]; then
    FLAGS+=(-fsanitize=address,undefined -fno-omit-frame-pointer)
fi
clang "${FLAGS[@]}" -Dqcow2_extract_fd=test_extract -c "$ROOT/host/image/raw_cache.c" -o "$WORK/raw_cache.o"
clang "${FLAGS[@]}" "$ROOT/tests/host/test_raw_cache.c" "$ROOT/host/image/disk.c" \
    "$ROOT/host/image/qcow2.c" "$WORK/raw_cache.o" -lz -o "$WORK/test"
mkdir "$WORK/data"
"$WORK/test" "$WORK/data"
