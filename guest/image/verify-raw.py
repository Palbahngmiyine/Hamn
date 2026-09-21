#!/usr/bin/env python3
"""Check fixed virtual size, protective MBR and GPT CRCs after decoder comparison."""
import pathlib
import struct
import sys
import zlib

image = pathlib.Path(sys.argv[1])
if image.stat().st_size != 8 * 1024**3:
    raise SystemExit("guest raw virtual size must be 8 GiB")
with image.open("rb") as source:
    mbr = source.read(512)
    if mbr[510:512] != b"\x55\xaa" or mbr[450] != 0xEE:
        raise SystemExit("invalid protective MBR")
    source.seek(512)
    header = bytearray(source.read(512))
    size, expected_crc = struct.unpack_from("<II", header, 12)
    if header[:8] != b"EFI PART" or not 92 <= size <= 512:
        raise SystemExit("invalid GPT header")
    struct.pack_into("<I", header, 16, 0)
    if zlib.crc32(header[:size]) != expected_crc:
        raise SystemExit("invalid GPT header CRC")
    table_lba, entries, entry_size, entries_crc = struct.unpack_from("<QIII", header, 72)
    table_size = entries * entry_size
    if not entries or entry_size < 128 or table_size > 16 * 1024**2:
        raise SystemExit("invalid GPT partition table bounds")
    source.seek(table_lba * 512)
    table = source.read(table_size)
    if len(table) != table_size or zlib.crc32(table) != entries_crc:
        raise SystemExit("invalid GPT partition table CRC")
    if not any(table):
        raise SystemExit("empty GPT partition table")
