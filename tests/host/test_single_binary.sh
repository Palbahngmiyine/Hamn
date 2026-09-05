#!/bin/bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
binary=${HAMN:-$root/build/hamn}
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
bash "$root/scripts/check-host-binary.sh" "$binary"
cp "$binary" "$temporary/hamn"
expected=$("$binary" --version)
actual=$(cd "$temporary" && ./hamn --version)
[ "$expected" = "$actual" ]
"$temporary/hamn" --help >/dev/null
echo 'single binary relocation: passed'
