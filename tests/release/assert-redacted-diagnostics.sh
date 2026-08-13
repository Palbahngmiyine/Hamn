#!/bin/bash
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "usage: $0 EXTRACTED_DIAGNOSTIC_DIR [CANARY_FILE]" >&2
    exit 2
fi

ROOT=$1
CANARY_FILE=${2:-}

[ -d "$ROOT" ] || {
    echo "FAIL: diagnostic input must be an extracted directory: $ROOT" >&2
    exit 1
}
if [ -n "$CANARY_FILE" ] && [ ! -f "$CANARY_FILE" ]; then
    echo "FAIL: canary file does not exist: $CANARY_FILE" >&2
    exit 1
fi
symlink=$(find "$ROOT" -type l -print -quit)
if [ -n "$symlink" ]; then
    echo "FAIL: diagnostic bundle contains a symlink: ${symlink#"$ROOT"/}" >&2
    exit 1
fi

report_leak() {
    category=$1
    file=$2
    relative=${file#"$ROOT"/}
    echo "FAIL: diagnostic bundle contains $category in $relative" >&2
    leaked=1
}

leaked=0
file_count=0
while IFS= read -r file; do
    file_count=$((file_count + 1))

    if LC_ALL=C grep -aEql -- \
        '-----BEGIN ([A-Z0-9]+ )?PRIVATE KEY-----' "$file"; then
        report_leak "a private key" "$file"
    fi
    if LC_ALL=C grep -aEql -- \
        '(^|[^[:alnum:]_])(Authorization:[[:space:]]*Bearer|Bearer)[[:space:]]+[A-Za-z0-9._~+/-]{8,}' \
        "$file"; then
        report_leak "a bearer credential" "$file"
    fi
    if LC_ALL=C grep -aEql -- \
        '(^|[^[:alnum:]_])(token|access[_-]?token|refresh[_-]?token|password|client[-_]?key(-data)?|clientKeyData)[[:space:]"]*[:=][[:space:]]*"?[A-Za-z0-9+/_.=-]{4,}' \
        "$file"; then
        report_leak "a credential field" "$file"
    fi
    if LC_ALL=C grep -aEql -- \
        '(^|[^A-Z0-9])(AKIA|ASIA)[A-Z0-9]{16}([^A-Z0-9]|$)' "$file"; then
        report_leak "an AWS access key" "$file"
    fi
    if LC_ALL=C grep -aEql -- \
        '(^|[^A-Za-z0-9_-])[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}([^A-Za-z0-9_-]|$)' \
        "$file"; then
        report_leak "a JWT-like token" "$file"
    fi
    if LC_ALL=C grep -aEql -- \
        '^[[:space:]]*[[:xdigit:]]{32,}[[:space:]]*$' "$file"; then
        report_leak "an opaque hexadecimal credential" "$file"
    fi
    if LC_ALL=C grep -aEql -- \
        '^[[:space:]]*[A-Za-z0-9_+/-]{32,}={0,2}[[:space:]]*$' "$file"; then
        report_leak "an opaque encoded credential" "$file"
    fi
    if LC_ALL=C grep -aEq '^[[:space:]]*kind:[[:space:]]*Config[[:space:]]*$' "$file" &&
        LC_ALL=C grep -aEq '^[[:space:]]*clusters:[[:space:]]*$' "$file" &&
        LC_ALL=C grep -aEq '^[[:space:]]*users:[[:space:]]*$' "$file"; then
        report_leak "an embedded kubeconfig" "$file"
    fi

    if [ -n "$CANARY_FILE" ]; then
        while IFS= read -r canary || [ -n "$canary" ]; do
            [ -n "$canary" ] || continue
            if LC_ALL=C grep -aFql -- "$canary" "$file"; then
                report_leak "a secret canary" "$file"
                break
            fi
        done <"$CANARY_FILE"
    fi
done < <(find "$ROOT" -type f -print | LC_ALL=C sort)

[ "$file_count" -gt 0 ] || {
    echo "FAIL: diagnostic bundle is empty" >&2
    exit 1
}
[ "$leaked" -eq 0 ] || exit 1

echo "OK: diagnostic bundle contains no recognized credentials or kubeconfig"
