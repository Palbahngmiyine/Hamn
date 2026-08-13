#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)

if [ "$(uname -s)" != Linux ]; then
    echo "SKIP: mountInotify guest safety test requires Linux openat and virtiofs fixtures"
    exit 0
fi

WORK=$(mktemp -d)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

cc -std=gnu11 -Wall -Wextra -Werror=implicit-function-declaration \
    -D_GNU_SOURCE -DHAMN_TEST -I"$ROOT/agent" \
    "$ROOT/tests/test_mount_inotify.c" "$ROOT/agent/api/mount_inotify.c" \
    -o "$WORK/test-mount-inotify"
"$WORK/test-mount-inotify"
