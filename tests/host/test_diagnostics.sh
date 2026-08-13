#!/bin/bash
set -euo pipefail

HAMN=${HAMN:-build/hamn}
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

PROFILE="$WORK/.hamn/default"
LOGS="$PROFILE/logs"
mkdir -p "$LOGS" "$WORK/.kube"

TOKEN_CANARY=hamn-diagnostic-token-canary-9f31e7
KEY_CANARY=HamnPrivateKeyPayloadCanary9f31e7
SECRET_CANARY=hamn-arbitrary-secret-data-canary-9f31e7
KUBECONFIG_CANARY=hamn-kubeconfig-file-canary-9f31e7
BOUNDARY_CANARY=hamn-tail-boundary-canary-value
SYMLINK_CANARY=hamn-symlink-log-canary-value
MULTILINE_CANARY=hamn-multiline-credential-canary-value
DIRECTORY_SYMLINK_CANARY=hamn-log-directory-symlink-canary-value
ANSI_CANARY=hamn-ansi-obfuscated-token-canary-value
JSON_DATA_CANARY=hamn-json-data-before-kind-canary-value
PRIVATE_BLOCK_CANARY=HamnIndependentPrivateBlockCanary9f31e7
OPAQUE_HEX_CANARY=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
OPAQUE_BASE64URL_CANARY=abcdefghijklmnopqrstuvwxyz0123456789_-abcdefghijklmnop
OPAQUE_SHORT_CANARY=ghijklmnopqrstuvwxyz0123456789abcdefgh
BOOTSTRAP_TOKEN_CANARY=abcdef.0123456789abcdef

cat >"$WORK/canaries" <<EOF
$TOKEN_CANARY
$KEY_CANARY
$SECRET_CANARY
$KUBECONFIG_CANARY
$BOUNDARY_CANARY
$SYMLINK_CANARY
$MULTILINE_CANARY
$DIRECTORY_SYMLINK_CANARY
$ANSI_CANARY
$JSON_DATA_CANARY
$PRIVATE_BLOCK_CANARY
$OPAQUE_HEX_CANARY
$OPAQUE_BASE64URL_CANARY
$OPAQUE_SHORT_CANARY
$BOOTSTRAP_TOKEN_CANARY
EOF
chmod 0600 "$WORK/canaries"

cat >"$WORK/.kube/config" <<EOF
apiVersion: v1
kind: Config
clusters: []
users:
- name: diagnostic-test
  user:
    token: $KUBECONFIG_CANARY
current-context: diagnostic-test
EOF
chmod 0600 "$WORK/.kube/config"

cat >"$PROFILE/private-key.pem" <<EOF
-----BEGIN PRIVATE KEY-----
$KEY_CANARY
-----END PRIVATE KEY-----
EOF
chmod 0600 "$PROFILE/private-key.pem"

cat >"$LOGS/serial.log" <<EOF
serial console initialized safely
serial console resumed safely
$OPAQUE_HEX_CANARY
$OPAQUE_BASE64URL_CANARY
$OPAQUE_SHORT_CANARY
bootstrap handshake $BOOTSTRAP_TOKEN_CANARY accepted
bootstrap handshake abcde.1123456789abcdef preserved
bootstrap handshake ghijkl.0123456789abcde preserved
bootstrap handshake abcdefg.1123456789abcdef preserved
bootstrap handshake ghijkl.0123456789abcdef0 preserved
token: $TOKEN_CANARY
-----BEGIN PRIVATE KEY-----
$KEY_CANARY
-----END PRIVATE KEY-----
password:
  $MULTILINE_CANARY
serial console resumed after redaction safely
EOF
printf 'vmrun started safely\nto\033[31mken: %s\n' "$ANSI_CANARY" \
    >"$LOGS/vmrun.log"
cat >>"$LOGS/vmrun.log" <<EOF
Authorization: Bearer $TOKEN_CANARY
env:
  - name: arbitrary
    value: $SECRET_CANARY
kind: Secret
data:
  arbitrary-name: $SECRET_CANARY
vmrun completed after redaction safely
EOF
chmod 0600 "$LOGS/serial.log" "$LOGS/vmrun.log"

ARCHIVE="$WORK/output with spaces/nested/diagnostic archive.tar"
HOME="$WORK" "$HAMN" diagnostics create --path "$ARCHIVE" --output json \
    >"$WORK/result.json" 2>"$WORK/result.err"
grep -q '"schemaVersion":1' "$WORK/result.json"
grep -q '"operation":"diagnostics.create"' "$WORK/result.json"
grep -q '"format":"ustar"' "$WORK/result.json"
grep -q '"redacted":true' "$WORK/result.json"
grep -Fq "$ARCHIVE" "$WORK/result.json"
test ! -s "$WORK/result.err"
test -f "$ARCHIVE"
test "$(stat -f '%Lp' "$ARCHIVE")" = 600
test "$(stat -f '%Lp' "$WORK/output with spaces/nested")" = 700
if find "$WORK/output with spaces" -name '*.tmp.*' -print -quit | grep -q .; then
    fail "diagnostic creation left a temporary archive"
fi

tar -tf "$ARCHIVE" | LC_ALL=C sort >"$WORK/members.actual"
cat >"$WORK/members.expected" <<'EOF'
logs/serial.log
logs/vmrun.log
manifest.json
status.json
EOF
cmp "$WORK/members.expected" "$WORK/members.actual"

mkdir "$WORK/extracted"
tar -xf "$ARCHIVE" -C "$WORK/extracted"
"$SCRIPT_DIR/../release/assert-redacted-diagnostics.sh" \
    "$WORK/extracted" "$WORK/canaries" >/dev/null
grep -q '^serial console initialized safely$' \
    "$WORK/extracted/logs/serial.log"
grep -q '^serial console resumed safely$' \
    "$WORK/extracted/logs/serial.log"
grep -q '^vmrun started safely$' "$WORK/extracted/logs/vmrun.log"
grep -q '^serial console resumed after redaction safely$' \
    "$WORK/extracted/logs/serial.log"
grep -q '^bootstrap handshake abcde\.1123456789abcdef preserved$' \
    "$WORK/extracted/logs/serial.log"
grep -q '^bootstrap handshake ghijkl\.0123456789abcde preserved$' \
    "$WORK/extracted/logs/serial.log"
grep -q '^bootstrap handshake abcdefg\.1123456789abcdef preserved$' \
    "$WORK/extracted/logs/serial.log"
grep -q '^bootstrap handshake ghijkl\.0123456789abcdef0 preserved$' \
    "$WORK/extracted/logs/serial.log"
grep -q '^vmrun completed after redaction safely$' \
    "$WORK/extracted/logs/vmrun.log"
grep -q '^\[REDACTED sensitive log line\]$' \
    "$WORK/extracted/logs/serial.log"
grep -q '"schemaVersion":1' "$WORK/extracted/status.json"
grep -q '"dockerContext":"hamn"' "$WORK/extracted/status.json"
grep -q '"enabled":false' "$WORK/extracted/status.json"
grep -q '"collectionPolicy":"allowlisted metadata and bounded log tails"' \
    "$WORK/extracted/manifest.json"

# Existing files and symlinks are never replaced by archive publication.
ARCHIVE_HASH=$(shasum -a 256 "$ARCHIVE")
if HOME="$WORK" "$HAMN" diagnostics create --path "$ARCHIVE" \
    >"$WORK/existing.out" 2>"$WORK/existing.err"; then
    fail "diagnostics replaced an existing archive"
fi
grep -q 'cannot create diagnostic archive' "$WORK/existing.err"
test "$ARCHIVE_HASH" = "$(shasum -a 256 "$ARCHIVE")"

printf '%s\n' outside-unchanged >"$WORK/outside"
ln -s "$WORK/outside" "$WORK/output-link.tar"
if HOME="$WORK" "$HAMN" diagnostics create --path "$WORK/output-link.tar" \
    >"$WORK/output-link.out" 2>"$WORK/output-link.err"; then
    fail "diagnostics replaced an output symlink"
fi
test "$(cat "$WORK/outside")" = outside-unchanged

# A partial first line in a bounded tail is dropped, so a field name that fell
# outside the read window cannot leave its value behind.
{
    printf 'token: '
    dd if=/dev/zero bs=1024 count=140 2>/dev/null | tr '\0' x
    printf '%s\n' "$BOUNDARY_CANARY"
    printf 'tail boundary completed safely\n'
} >"$LOGS/serial.log"
cat >"$LOGS/vmrun.log" <<EOF
vmrun JSON diagnostic line safely
{
  "data" : {
    "opaque": "$JSON_DATA_CANARY"
  },
  "kind": "Secret"
}
EOF
BOUNDARY_ARCHIVE="$WORK/boundary.tar"
HOME="$WORK" "$HAMN" diagnostics create --path "$BOUNDARY_ARCHIVE" \
    >"$WORK/boundary.json"
mkdir "$WORK/boundary"
tar -xf "$BOUNDARY_ARCHIVE" -C "$WORK/boundary"
"$SCRIPT_DIR/../release/assert-redacted-diagnostics.sh" \
    "$WORK/boundary" "$WORK/canaries" >/dev/null
grep -q '^tail boundary completed safely$' \
    "$WORK/boundary/logs/serial.log"

# Log symlinks are not followed, even when their target contains a canary.
printf '%s\n' "$SYMLINK_CANARY" >"$WORK/outside-secret-log"
rm "$LOGS/serial.log"
ln -s "$WORK/outside-secret-log" "$LOGS/serial.log"
cat >"$LOGS/vmrun.log" <<EOF
vmrun private block diagnostic line safely
-----BEGIN PRIVATE KEY-----
$PRIVATE_BLOCK_CANARY
-----END PRIVATE KEY-----
vmrun after private block safely
EOF
SYMLINK_ARCHIVE="$WORK/symlink-log.tar"
HOME="$WORK" "$HAMN" diagnostics create --path "$SYMLINK_ARCHIVE" \
    >"$WORK/symlink-log.json"
mkdir "$WORK/symlink-log"
tar -xf "$SYMLINK_ARCHIVE" -C "$WORK/symlink-log"
"$SCRIPT_DIR/../release/assert-redacted-diagnostics.sh" \
    "$WORK/symlink-log" "$WORK/canaries" >/dev/null
grep -q '^(log unavailable)$' "$WORK/symlink-log/logs/serial.log"
grep -q '^vmrun after private block safely$' \
    "$WORK/symlink-log/logs/vmrun.log"

# An intermediate logs-directory symlink is also rejected.
rm "$LOGS/serial.log" "$LOGS/vmrun.log"
rmdir "$LOGS"
mkdir "$WORK/outside-logs"
printf '%s\n' "$DIRECTORY_SYMLINK_CANARY" \
    >"$WORK/outside-logs/serial.log"
printf '%s\n' "$DIRECTORY_SYMLINK_CANARY" \
    >"$WORK/outside-logs/vmrun.log"
ln -s "$WORK/outside-logs" "$LOGS"
DIRECTORY_SYMLINK_ARCHIVE="$WORK/symlink-log-directory.tar"
HOME="$WORK" "$HAMN" diagnostics create \
    --path "$DIRECTORY_SYMLINK_ARCHIVE" >"$WORK/symlink-log-directory.json"
mkdir "$WORK/symlink-log-directory"
tar -xf "$DIRECTORY_SYMLINK_ARCHIVE" -C "$WORK/symlink-log-directory"
"$SCRIPT_DIR/../release/assert-redacted-diagnostics.sh" \
    "$WORK/symlink-log-directory" "$WORK/canaries" >/dev/null
grep -q '^(log unavailable)$' \
    "$WORK/symlink-log-directory/logs/serial.log"
grep -q '^(log unavailable)$' \
    "$WORK/symlink-log-directory/logs/vmrun.log"

# Without --path, the command writes a mode-0600 archive below ~/.hamn.
DEFAULT_HOME="$WORK/default-home"
mkdir -p "$DEFAULT_HOME"
HOME="$DEFAULT_HOME" "$HAMN" diagnostics create \
    >"$WORK/default-result.json"
DEFAULT_PATH=$(sed -n 's/.*"path":"\([^"]*\)".*/\1/p' \
    "$WORK/default-result.json")
test -n "$DEFAULT_PATH"
case "$DEFAULT_PATH" in
"$DEFAULT_HOME/.hamn/diagnostics/"*.tar) ;;
*) fail "default diagnostic path escaped ~/.hamn/diagnostics" ;;
esac
test -f "$DEFAULT_PATH"
test "$(stat -f '%Lp' "$DEFAULT_PATH")" = 600

if HOME="$WORK" "$HAMN" diagnostics create --path \
    >"$WORK/missing-path.out" 2>"$WORK/missing-path.err"; then
    fail "diagnostics accepted a missing --path value"
fi
grep -q 'usage: hamn diagnostics create' "$WORK/missing-path.err"

echo "OK: diagnostic archives are bounded, atomic, and credential-redacted"
