#!/bin/bash
set -euo pipefail
binary=${1:?usage: check-host-binary.sh BINARY}
file "$binary" | grep -q 'Mach-O 64-bit executable'
codesign --verify --strict "$binary"
otool -l "$binary" | grep -q LC_UUID
while IFS= read -r dependency; do
    case "$dependency" in
        /usr/lib/*|/System/Library/*) ;;
        *) echo "unexpected runtime dependency: $dependency" >&2; exit 1 ;;
    esac
done < <(otool -L "$binary" | tail -n +2 | awk '{print $1}')
codesign -d --entitlements :- "$binary" 2>/dev/null |
    grep -q 'com.apple.security.virtualization'
