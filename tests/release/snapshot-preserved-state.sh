#!/bin/bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 STATE_ROOT OUTPUT_MANIFEST" >&2
    exit 2
fi

ROOT=$1
OUTPUT=$2
[ -d "$ROOT" ] || {
    echo "FAIL: state root does not exist: $ROOT" >&2
    exit 1
}

ROOT=$(cd "$ROOT" && pwd -P)
OUTPUT_DIR=$(cd "$(dirname "$OUTPUT")" && pwd -P)
OUTPUT="$OUTPUT_DIR/$(basename "$OUTPUT")"
case "$OUTPUT" in
    "$ROOT"/*)
        echo "FAIL: preservation manifest must be outside the state root" >&2
        exit 2
        ;;
esac

stat_mode() {
    if stat -f '%Lp' "$1" >/dev/null 2>&1; then
        stat -f '%Lp' "$1"
    else
        stat -c '%a' "$1"
    fi
}

stat_size() {
    if stat -f '%z' "$1" >/dev/null 2>&1; then
        stat -f '%z' "$1"
    else
        stat -c '%s' "$1"
    fi
}

sha256_digest() {
    case "$(uname -s)" in
        Darwin) shasum -a 256 "$@" ;;
        Linux) sha256sum "$@" ;;
        *)
            if command -v sha256sum >/dev/null 2>&1; then
                sha256sum "$@"
            elif command -v shasum >/dev/null 2>&1; then
                shasum -a 256 "$@"
            else
                echo "FAIL: no SHA-256 command is available" >&2
                return 1
            fi
            ;;
    esac
}

sha256_stream() {
    sha256_digest | awk '{print $1}'
}

hash_regular_file() {
    path=$1
    sha256_digest "$path" | awk '{print $1}'
}

tmp=$(mktemp "$OUTPUT_DIR/.hamn-preservation.XXXXXX")
trap 'rm -f "$tmp"' EXIT
(
    printf '# hamn-preserved-state-v2 full-sha256\n'
    cd "$ROOT"
    while IFS= read -r entry; do
        relative=${entry#./}
        case "$relative" in
            *$'\t'*)
                echo "FAIL: state path contains a tab" >&2
                exit 1
                ;;
        esac
        mode=$(stat_mode "$entry")
        if [ -L "$entry" ]; then
            target=$(readlink "$entry")
            digest=$(printf '%s' "$target" | sha256_stream)
            printf 'L\t%s\t-\t%s\t%s\n' "$mode" "$digest" "$relative"
        elif [ -f "$entry" ]; then
            size=$(stat_size "$entry")
            digest=$(hash_regular_file "$entry")
            printf 'F\t%s\t%s\t%s\t%s\n' \
                "$mode" "$size" "$digest" "$relative"
        elif [ -d "$entry" ]; then
            printf 'D\t%s\t-\t-\t%s\n' "$mode" "$relative"
        else
            printf 'O\t%s\t-\t-\t%s\n' "$mode" "$relative"
        fi
    done < <(LC_ALL=C find . -mindepth 1 -print | LC_ALL=C sort)
) >"$tmp"
chmod 0600 "$tmp"
mv -f "$tmp" "$OUTPUT"
trap - EXIT

echo "OK: wrote preservation manifest: $OUTPUT"
