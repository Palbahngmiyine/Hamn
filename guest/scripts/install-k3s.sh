#!/bin/bash
set -euo pipefail
export LC_ALL=C

# K3s is optional. Its binary and air-gap image archive are accepted only when
# a Hamn compatibility manifest, signed by the release key embedded in the
# immutable guest image, pins both HTTPS artifacts and their SHA-256 values.
VERSION=${HAMN_K3S_VERSION:-v1.36.2+k3s1}
DEST=${HAMN_K3S_DEST:-/usr/local/bin/k3s}
AIRGAP_DIR=${HAMN_K3S_AIRGAP_DIR:-/var/lib/rancher/k3s/agent/images}
AIRGAP_NAME=k3s-airgap-images-arm64.tar.zst
MANIFEST=${HAMN_K3S_MANIFEST:-/etc/hamn/k3s-compatibility.json}
SIGNATURE=${HAMN_K3S_MANIFEST_SIGNATURE:-$MANIFEST.sig}
PUBLIC_KEY=${HAMN_K3S_PUBLIC_KEY:-/etc/hamn/hamn-release.pub}
CURL=${HAMN_CURL:-curl}
SHA256SUM=${HAMN_SHA256SUM:-sha256sum}
ALLOW_LOCAL=${HAMN_K3S_ALLOW_LOCAL_ARTIFACTS:-0}
ARCH=${HAMN_K3S_ARCH:-$(uname -m)}

fail() {
    echo "hamn: k3s installation: $*" >&2
    exit 1
}

safe_regular() {
    [ -f "$1" ] && [ ! -L "$1" ]
}

safe_existing_or_absent() {
    if [ ! -e "$1" ] && [ ! -L "$1" ]; then
        return 0
    fi
    safe_regular "$1"
}

sha256_file() {
    "$SHA256SUM" "$1" | awk '{print $1}'
}

fetch() {
    local source=$1 destination=$2
    case "$source" in
    https://*)
        "$CURL" --fail --location --proto '=https' --tlsv1.2 --retry 3 \
            --output "$destination" "$source"
        ;;
    file://*|/*)
        [ "$ALLOW_LOCAL" = 1 ] || fail "local K3s artifacts are disabled"
        local path=${source#file://}
        safe_regular "$path" || fail "local K3s artifact is unsafe"
        cp "$path" "$destination"
        ;;
    *) fail "K3s artifact URL must use HTTPS" ;;
    esac
}

verify_manifest() {
    safe_regular "$MANIFEST" || fail "compatibility manifest is unavailable"
    safe_regular "$SIGNATURE" || fail "compatibility manifest signature is unavailable"
    safe_regular "$PUBLIC_KEY" || fail "release public key is unavailable"
    ssh-keygen -lf "$PUBLIC_KEY" | grep -q 'ED25519' ||
        fail "release public key is not Ed25519"

    local allowed
    allowed=$(mktemp "${TMPDIR:-/tmp}/hamn-k3s-allowed.XXXXXX") ||
        fail "cannot create manifest verifier"
    printf 'hamn-release ' >"$allowed"
    cat "$PUBLIC_KEY" >>"$allowed"
    if ! ssh-keygen -Y verify -f "$allowed" -I hamn-release \
        -n hamn-k3s-compatibility -s "$SIGNATURE" <"$MANIFEST" >/dev/null; then
        rm -f "$allowed"
        fail "compatibility manifest signature verification failed"
    fi
    rm -f "$allowed"

    python3 - "$MANIFEST" "$VERSION" "$ALLOW_LOCAL" <<'PY'
import json
import re
import sys


def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise ValueError("duplicate key: " + key)
        result[key] = value
    return result


def artifact(value, name, allow_local):
    if not isinstance(value, dict) or set(value) != {"url", "sha256"}:
        raise ValueError(name + " artifact schema is invalid")
    url = value["url"]
    digest = value["sha256"]
    if not isinstance(url, str) or not url or any(ord(ch) < 33 or ord(ch) > 126 for ch in url):
        raise ValueError(name + " URL is invalid")
    if not (url.startswith("https://") or
            (allow_local and (url.startswith("file://") or url.startswith("/")))):
        raise ValueError(name + " URL must use HTTPS")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError(name + " SHA-256 is invalid")
    return url, digest


try:
    with open(sys.argv[1], encoding="utf-8") as source:
        value = json.load(source, object_pairs_hook=pairs,
                          parse_constant=lambda text: (_ for _ in ()).throw(ValueError(text)))
    if not isinstance(value, dict) or set(value) != {
            "schemaVersion", "version", "architecture", "binary", "airgapImages"}:
        raise ValueError("compatibility manifest schema is invalid")
    if value["schemaVersion"] != 1 or value["architecture"] != "arm64" or \
            value["version"] != sys.argv[2] or \
            not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+\+k3s[0-9]+", value["version"]):
        raise ValueError("compatibility manifest is not supported")
    binary = artifact(value["binary"], "K3s binary", sys.argv[3] == "1")
    airgap = artifact(value["airgapImages"], "K3s air-gap images", sys.argv[3] == "1")
except (OSError, TypeError, ValueError, json.JSONDecodeError) as error:
    raise SystemExit("hamn: k3s installation: invalid compatibility manifest: " + str(error))

print(binary[0])
print(binary[1])
print(airgap[0])
print(airgap[1])
PY
}

[ "$ARCH" = aarch64 ] ||
    fail "K3s bundle currently supports the arm64 guest only"

fields_file=$(mktemp "${TMPDIR:-/tmp}/hamn-k3s-fields.XXXXXX") ||
    fail "cannot stage compatibility manifest fields"
cleanup() {
    rm -f "$fields_file"
    [ -z "${binary_tmp:-}" ] || rm -f "$binary_tmp"
    [ -z "${airgap_tmp:-}" ] || rm -f "$airgap_tmp"
}
trap cleanup EXIT
verify_manifest >"$fields_file"
{
    IFS= read -r binary_url
    IFS= read -r binary_hash
    IFS= read -r airgap_url
    IFS= read -r airgap_hash
    IFS= read -r unexpected || true
} <"$fields_file"
[ -n "$binary_url" ] && [ -n "$binary_hash" ] && [ -n "$airgap_url" ] &&
    [ -n "$airgap_hash" ] && [ -z "${unexpected:-}" ] ||
    fail "compatibility manifest fields are incomplete"

install -d -m 0755 "$(dirname "$DEST")" "$AIRGAP_DIR"
airgap_dest=$AIRGAP_DIR/$AIRGAP_NAME
safe_existing_or_absent "$DEST" || fail "K3s binary destination is unsafe"
safe_existing_or_absent "$airgap_dest" ||
    fail "K3s air-gap destination is unsafe"

binary_tmp=
airgap_tmp=
if [ ! -x "$DEST" ] || [ "$(sha256_file "$DEST")" != "$binary_hash" ]; then
    binary_tmp=$(mktemp "$(dirname "$DEST")/.hamn-k3s.XXXXXX") ||
        fail "cannot stage K3s binary"
    fetch "$binary_url" "$binary_tmp"
    [ "$(sha256_file "$binary_tmp")" = "$binary_hash" ] ||
        fail "K3s binary SHA-256 mismatch"
fi
if [ ! -f "$airgap_dest" ] ||
   [ "$(sha256_file "$airgap_dest")" != "$airgap_hash" ]; then
    airgap_tmp=$(mktemp "$AIRGAP_DIR/.hamn-k3s-airgap.XXXXXX") ||
        fail "cannot stage K3s air-gap images"
    fetch "$airgap_url" "$airgap_tmp"
    [ "$(sha256_file "$airgap_tmp")" = "$airgap_hash" ] ||
        fail "K3s air-gap image SHA-256 mismatch"
fi

if [ -n "$binary_tmp" ]; then
    chmod 0755 "$binary_tmp"
    mv -f "$binary_tmp" "$DEST"
    binary_tmp=
fi
if [ -n "$airgap_tmp" ]; then
    chmod 0644 "$airgap_tmp"
    mv -f "$airgap_tmp" "$airgap_dest"
    airgap_tmp=
fi

[ "$(sha256_file "$DEST")" = "$binary_hash" ] ||
    fail "installed K3s binary SHA-256 mismatch"
[ "$(sha256_file "$airgap_dest")" = "$airgap_hash" ] ||
    fail "installed K3s air-gap image SHA-256 mismatch"
"$DEST" --version 2>/dev/null | grep -Fq "$VERSION" ||
    fail "installed K3s version does not match compatibility manifest"
