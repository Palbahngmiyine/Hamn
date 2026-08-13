#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CONFIG=$ROOT/host/vz/vz_config.m

fail() {
    echo "FAIL: nested virtualization contract: $*" >&2
    exit 1
}

# Apple documents `isNestedVirtualizationSupported` as available only on Macs
# with an M3 chip or later. The capability query, rather than model-name
# parsing, is the authoritative runtime guard for future supported hardware.
grep -Fq '@available(macOS 15.0, *)' "$CONFIG" ||
    fail "macOS 15 availability guard is missing"
grep -Fq 'isNestedVirtualizationSupported' "$CONFIG" ||
    fail "Apple nested-virtualization capability guard is missing"
grep -Fq 'nestedVirtualizationEnabled = YES' "$CONFIG" ||
    fail "supported nested virtualization is not enabled"
grep -Fq 'M3 or later' "$CONFIG" ||
    fail "unsupported hardware error does not describe the M3 boundary"

grep -Fq 'M3 chip or' "$ROOT/docs/ARCHITECTURE.md" ||
    fail "English architecture documentation omits the M3 boundary"
grep -Fq 'M3 칩 이상' "$ROOT/docs/ARCHITECTURE.ko.md" ||
    fail "Korean architecture documentation omits the M3 boundary"
grep -Fq 'M3-or-later Mac' "$ROOT/docs/CONFIGURATION.md" ||
    fail "English configuration documentation omits the M3 boundary"
grep -Fq 'M3 칩 이상 Mac' "$ROOT/docs/CONFIGURATION.ko.md" ||
    fail "Korean configuration documentation omits the M3 boundary"

echo "PASS: nested virtualization is capability-gated for macOS 15 and M3-or-later Macs"
