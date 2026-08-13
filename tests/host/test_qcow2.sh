#!/bin/bash
# S1 검증: qcow2 → raw 변환 결과의 구조 검사 + (가능하면) qemu-img 교차검증.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: test_qcow2.sh SIGNED_GUEST_IMAGE" >&2
    exit 2
fi

IMG=$1
HAMN=${HAMN:-build/hamn}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
OUT="$WORK/disk.raw"

if [ ! -f "$IMG" ]; then
    echo "SKIP: signed guest image fixture not found: $IMG" >&2
    exit 1
fi

"$HAMN" qcow2-extract "$IMG" "$OUT"

# 1) 가상 크기 일치 (qcow2 헤더 offset 24의 BE64)
virt_size=$(python3 -c "
import struct, sys
with open('$IMG','rb') as f:
    h = f.read(32)
print(struct.unpack('>Q', h[24:32])[0])
")
raw_size=$(stat -f %z "$OUT")
[ "$virt_size" = "$raw_size" ] || { echo "FAIL: size $raw_size != virtual $virt_size"; exit 1; }

# 2) protective MBR 부트 시그니처
sig=$(xxd -p -s 510 -l 2 "$OUT")
[ "$sig" = "55aa" ] || { echo "FAIL: no MBR boot signature (got $sig)"; exit 1; }

# 3) GPT 헤더 (LBA 1)
gpt=$(dd if="$OUT" bs=1 skip=512 count=8 2>/dev/null)
[ "$gpt" = "EFI PART" ] || { echo "FAIL: no GPT header (got '$gpt')"; exit 1; }

# 4) qemu-img가 있으면 sha256 교차검증
if command -v qemu-img >/dev/null 2>&1; then
    REF="$WORK/ref.raw"
    qemu-img convert -O raw "$IMG" "$REF"
    h1=$(shasum -a 256 "$OUT" | cut -d' ' -f1)
    h2=$(shasum -a 256 "$REF" | cut -d' ' -f1)
    [ "$h1" = "$h2" ] || { echo "FAIL: sha256 differs from qemu-img"; exit 1; }
    echo "OK: qemu-img sha256 cross-check passed"
else
    echo "OK: structural checks passed (qemu-img unavailable, cross-check skipped)"
fi
