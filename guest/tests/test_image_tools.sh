#!/bin/bash
# CLI contract of hamn-image-tool as the image builder and release publication
# call it: exit status 0/1/2, one prefixed stderr line, and option parsing.
# Module behavior is covered by test_image_tool.c; fixtures here are tiny or
# sparse and establish no image-size or VM claims.
set -euo pipefail
trap 'echo "FAIL: ${BASH_SOURCE[0]}:$LINENO: $BASH_COMMAND" >&2' ERR

TOOL=${1:?usage: test_image_tools.sh HAMN_IMAGE_TOOL}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
chmod 0700 "$WORK"
umask 022

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

# expect STATUS TEXT COMMAND...: exact exit status and TEXT on stderr.
expect() {
    local status=$1 text=$2 actual=0
    shift 2
    "$@" >"$WORK/out" 2>"$WORK/err" || actual=$?
    if [ "$actual" -ne "$status" ] || ! grep -Fq -- "$text" "$WORK/err"; then
        echo "FAIL: expected status $status and '$text' from: $*" >&2
        echo "  got status $actual: $(cat "$WORK/err")" >&2
        exit 1
    fi
}

A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb

expect 2 'usage: hamn-image-tool' "$TOOL"
expect 2 'usage: hamn-image-tool' "$TOOL" unknown
expect 2 'usage: hamn-image-tool' "$TOOL" evidence
expect 2 'usage: hamn-image-tool' "$TOOL" evidence publish only-one
expect 2 'usage: hamn-image-tool' "$TOOL" verify-raw
expect 2 'usage: hamn-image-tool' "$TOOL" verify-release-size a b
expect 2 'required: --baseline' "$TOOL" verify-size
expect 2 'unknown option: --bogus' "$TOOL" verify-size --bogus x
expect 2 'repeated option: --report' "$TOOL" verify-size --report a --report b
expect 2 '--review-only takes no value' "$TOOL" verify-size --review-only=1
expect 2 '--budget requires a value' "$TOOL" verify-size --budget
expect 2 'unexpected argument: stray' "$TOOL" verify-size stray
expect 2 'must be decimal integers' "$TOOL" variations --seed 0x10 \
    --source-root . --baseline b --size-report r --output-directory o

# Size gate: 64 MiB of savings exactly, through both option spellings.
dd if=/dev/zero of="$WORK/baseline" bs=1 count=0 seek=67117056 2>/dev/null
dd if=/dev/zero of="$WORK/candidate" bs=1 count=0 seek=8192 2>/dev/null
printf 'docker.io\t28.0\t12345\n' >"$WORK/packages.tsv"
size_gate() {
    "$TOOL" verify-size --baseline="$WORK/baseline" --candidate "$WORK/candidate" \
        --packages-before "$WORK/packages.tsv" --packages-after="$WORK/packages.tsv" \
        --report "$WORK/report.json" --budget "$WORK/budget.json" \
        --base-sha256 "$A" --source-revision "$B" "$@"
}
expect 1 'hamn guest image size gate: reviewed release-size-budget.json is required' size_gate
test -f "$WORK/report.json"
size_gate --review-only
test -f "$WORK/report.budget-proposal.json"
expect 1 'hamn guest image release size gate: review-only or invalid image report cannot be published' \
    "$TOOL" verify-release-size "$WORK/candidate" "$WORK/report.json" \
    "$WORK/report.budget-proposal.json"
cp "$WORK/report.budget-proposal.json" "$WORK/budget.json"
size_gate
grep -Fxq '  "reviewOnly": false' "$WORK/report.json"
grep -Fxq "  \"imageSha256\": \"$(sha256 "$WORK/candidate")\"," "$WORK/report.json"
"$TOOL" verify-release-size "$WORK/candidate" "$WORK/report.json" "$WORK/budget.json"
"$TOOL" verify-release-size "$WORK/candidate" "$WORK/report.json" "$WORK/budget.json" "$B"
expect 1 'hamn guest image release size gate: guest image size report belongs to a different source revision' \
    "$TOOL" verify-release-size "$WORK/candidate" "$WORK/report.json" \
    "$WORK/budget.json" cccccccccccccccccccccccccccccccccccccccc
expect 1 'hamn guest image size gate: invalid source identity' \
    "$TOOL" verify-size --baseline x --candidate x --packages-before x \
    --packages-after x --report "$WORK/r2.json" --budget x \
    --base-sha256 "A${A#a}" --source-revision "$B"
test ! -e "$WORK/r2.json"

# Baseline evidence export.
printf 'baseline evidence bytes' >"$WORK/source.img"
printf '{"baselineSha256": "%s", "baselineCompressedBytes": %d}\n' \
    "$(sha256 "$WORK/source.img")" "$(wc -c <"$WORK/source.img" | tr -d ' ')" \
    >"$WORK/evidence-report.json"
"$TOOL" evidence check "$WORK/export.img" "$WORK/candidate" "$WORK/other"
expect 1 'hamn baseline evidence: baseline output collides with an existing or reserved artifact' \
    "$TOOL" evidence check "$WORK/export.img" "$WORK/export.img.sha256"
"$TOOL" evidence publish "$WORK/source.img" "$WORK/export.img" "$WORK/evidence-report.json"
cmp "$WORK/source.img" "$WORK/export.img"
[ "$(cat "$WORK/export.img.sha256")" = "$(sha256 "$WORK/source.img")  export.img" ]
expect 1 'hamn baseline evidence: baseline output collides' \
    "$TOOL" evidence publish "$WORK/source.img" "$WORK/export.img" "$WORK/evidence-report.json"

# Raw GPT check keeps the Python tool's unprefixed messages.
expect 1 'guest raw virtual size must be 8 GiB' "$TOOL" verify-raw "$WORK/candidate"
[ "$(cat "$WORK/err")" = 'guest raw virtual size must be 8 GiB' ]
expect 1 'cannot inspect' "$TOOL" verify-raw "$WORK/missing.raw"

# Variation generation validates its plan before any host or output work.
expect 1 'hamn image variations: seed must be uint32 and case count must be between one and four' \
    "$TOOL" variations --source-root "$WORK" --baseline "$WORK/baseline" \
    --size-report "$WORK/report.json" --output-directory "$WORK/variations" \
    --case-count 5
expect 1 'hamn image variations: seed must be uint32' \
    "$TOOL" variations --source-root "$WORK" --baseline "$WORK/baseline" \
    --size-report "$WORK/report.json" --output-directory "$WORK/variations" \
    --seed 4294967296
if [ "$(uname -s)" != Linux ]; then
    expect 1 'hamn image variations: real image variations require Linux arm64' \
        "$TOOL" variations --source-root "$WORK" --baseline "$WORK/baseline" \
        --size-report "$WORK/report.json" --output-directory "$WORK/variations"
fi
test ! -e "$WORK/variations"

echo "PASS: hamn-image-tool exit statuses, messages and option parsing"
