#!/bin/bash
# A fresh curl-style bootstrap must accept only immutable release metadata,
# install exact candidate bytes, and preserve the generation on failure.
set -euo pipefail
unset GITHUB_ACTIONS GITHUB_REPOSITORY GITHUB_RUN_ID GITHUB_RUN_ATTEMPT

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-artifacts.XXXXXX)
CACHED_INPUT=$(mktemp "$ROOT/packaging/release/.artifact-fixture.XXXXXX.log")
cleanup() {
    rm -f "$CACHED_INPUT"
    rm -rf "$WORK"
    make -C "$ROOT" host VERSION=0.0.1-dev >/dev/null
}
trap cleanup EXIT

sha256() {
    shasum -a 256 "$1" | awk '{print $1}'
}

printf 'preconfigured guest image fixture\n' >"$WORK/guest.img"
RELEASE_REF=$(git -C "$ROOT" rev-parse HEAD)
FAKE_BIN=$WORK/fake-bin
mkdir "$FAKE_BIN"
cat >"$FAKE_BIN/uname" <<'EOF'
#!/bin/sh
[ "$1" = -m ] || exit 2
printf 'x86_64\n'
EOF
chmod 0755 "$FAKE_BIN/uname"
if PATH="$FAKE_BIN:$PATH" \
RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
OUTPUT_DIR="$WORK/non-arm64-candidate" \
HAMN_GUEST_IMAGE="$WORK/guest.img" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/build-candidate.sh" >"$WORK/non-arm64.out" \
    2>"$WORK/non-arm64.err"; then
    echo "FAIL: candidate builder accepted a non-arm64 host" >&2
    exit 1
fi
grep -Fq 'release candidate must build on Apple Silicon arm64' \
    "$WORK/non-arm64.err"
CANONICAL_REPOSITORY=example/hamn
CANONICAL_MANIFEST_URL="https://github.com/$CANONICAL_REPOSITORY/releases/latest/download/hamn-update-manifest.json"
GITHUB_REPOSITORY="$CANONICAL_REPOSITORY" \
RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
OUTPUT_DIR="$WORK/canonical-candidate" \
HAMN_GUEST_IMAGE="$WORK/guest.img" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/build-candidate.sh" >"$WORK/canonical-candidate.out"
CANONICAL_HOST_ARTIFACT=$WORK/canonical-candidate/hamn-v0.0.1-darwin-arm64.tar.gz
[ "$(tar -xOf "$CANONICAL_HOST_ARTIFACT" \
    hamn-v0.0.1-darwin-arm64/packaging/release/update-manifest-url)" = \
    "$CANONICAL_MANIFEST_URL" ] || {
    echo "FAIL: GitHub candidate did not embed the canonical stable manifest URL" >&2
    exit 1
}
grep -Fq "readonly HAMN_VERSION=\"v0.0.1\"" \
    "$WORK/canonical-candidate/install.sh" || {
    echo "FAIL: GitHub candidate installer did not embed the release version" >&2
    exit 1
}
if GITHUB_REPOSITORY="$CANONICAL_REPOSITORY" \
RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
OUTPUT_DIR="$WORK/rejected-candidate" \
HAMN_GUEST_IMAGE="$WORK/guest.img" \
HAMN_RELEASE_MANIFEST_URL=https://downloads.example.invalid/other/manifest.json \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/build-candidate.sh" >"$WORK/rejected-candidate.out" \
    2>"$WORK/rejected-candidate.err"; then
    echo "FAIL: candidate accepted a manifest URL outside its canonical stable release" >&2
    exit 1
fi
grep -Fq 'HAMN_RELEASE_MANIFEST_URL must match the canonical GitHub Release manifest URL' \
    "$WORK/rejected-candidate.err"
RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
OUTPUT_DIR="$WORK/candidate" \
HAMN_GUEST_IMAGE="$WORK/guest.img" \
HAMN_RELEASE_MANIFEST_URL="file://$WORK/manifest.json" \
HAMN_RELEASE_ALLOW_LOCAL=1 \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/build-candidate.sh" >"$WORK/candidate.out"

HOST_ARTIFACT=$WORK/candidate/hamn-v0.0.1-darwin-arm64.tar.gz
GUEST_ARTIFACT=$WORK/candidate/hamn-v0.0.1-ubuntu-24.04-arm64.img
if tar -tzf "$HOST_ARTIFACT" | grep -Fq "$(basename "$CACHED_INPUT")"; then
    echo "FAIL: host artifact contains untracked local files" >&2
    exit 1
fi
if tar -tzf "$HOST_ARTIFACT" | grep -E '/(guest|shared|vendor)(/|$)' >/dev/null; then
    echo "FAIL: host artifact contains mutable guest build sources" >&2
    exit 1
fi
tar -tzf "$HOST_ARTIFACT" |
    grep -Fxq 'hamn-v0.0.1-darwin-arm64/packaging/release/physical-e2e.sh' ||
    {
        echo "FAIL: host artifact is missing its physical E2E harness" >&2
        exit 1
    }
if tar -tzf "$HOST_ARTIFACT" | grep -Fq '/colima-benchmark.sh'; then
    echo "FAIL: host artifact still contains a removed Colima benchmark harness" >&2
    exit 1
fi
EXTRACTED=$WORK/extracted
mkdir "$EXTRACTED"
tar -xzf "$HOST_ARTIFACT" -C "$EXTRACTED"
for harness in physical-e2e.sh; do
    path=$EXTRACTED/hamn-v0.0.1-darwin-arm64/packaging/release/$harness
    [ -x "$path" ] || {
        echo "FAIL: candidate release harness is not executable: $harness" >&2
        exit 1
    }
    "$path" --help >"$WORK/$harness.help"
    grep -Fq "usage: $harness" "$WORK/$harness.help" || {
        echo "FAIL: candidate release harness did not run: $harness" >&2
        exit 1
    }
done
HOST_HASH=$(sha256 "$HOST_ARTIFACT")
GUEST_HASH=$(sha256 "$GUEST_ARTIFACT")
python3 - "$WORK/candidate/hamn-v0.0.1.spdx.json" "$HOST_HASH" "$GUEST_HASH" <<'PY'
import json
import re
import sys

path, host_hash, guest_hash = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    sbom = json.load(source)
creation = sbom.get("creationInfo")
if not isinstance(creation, dict) or set(creation) != {
        "creators", "created", "licenseListVersion"} or \
        creation["creators"] != ["Tool: hamn-release-candidate"] or \
        not isinstance(creation["created"], str) or \
        not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z",
                         creation["created"]):
    raise SystemExit("SBOM creation metadata is invalid")
packages = sbom.get("packages")
if not isinstance(packages, list) or len(packages) != 2:
    raise SystemExit("SBOM packages are invalid")
checksums = {item["name"]: item["checksums"] for item in packages}
if checksums.get("hamn-v0.0.1-darwin-arm64.tar.gz") != [
        {"algorithm": "SHA256", "checksumValue": host_hash}] or \
        checksums.get("hamn-v0.0.1-ubuntu-24.04-arm64.img") != [
        {"algorithm": "SHA256", "checksumValue": guest_hash}]:
    raise SystemExit("SBOM hashes do not bind candidate artifacts")
PY
printf '%s' \
    '{"schemaVersion":2,"channel":"stable","version":"v0.0.1",' \
    '"commit":"'"$RELEASE_REF"'","validationMode":"github-hosted-no-vm",' \
    '"compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},' \
    '"artifacts":{"host":{"url":"file://'"$HOST_ARTIFACT"'","sha256":"'"$HOST_HASH"'"},' \
    '"guestImage":{"url":"file://'"$GUEST_ARTIFACT"'","sha256":"'"$GUEST_HASH"'"}}}' \
    >"$WORK/manifest.json"

HOME_DIR=$WORK/home
mkdir -p "$HOME_DIR"
HOME="$HOME_DIR" bash "$WORK/candidate/install.sh" --help >"$WORK/help.out"
grep -Fq 'For updates: hamn --headless system update --yes' "$WORK/help.out"
[ ! -e "$HOME_DIR/.local" ] && [ ! -e "$HOME_DIR/.hamn" ]
if HOME="$HOME_DIR" bash "$WORK/candidate/install.sh" --unknown >"$WORK/unknown.out" 2>"$WORK/unknown.err"; then
    echo "FAIL: bootstrap accepted an unknown option" >&2
    exit 1
fi
[ ! -e "$HOME_DIR/.local" ] && [ ! -e "$HOME_DIR/.hamn" ]
if HOME="$HOME_DIR" bash "$WORK/candidate/install.sh" >"$WORK/local.out" \
    2>"$WORK/local.err"; then
    echo "FAIL: bootstrap accepted local release input by default" >&2
    exit 1
fi
grep -Fq 'local artifacts are disabled' "$WORK/local.err"

HOME="$HOME_DIR" HAMN_INSTALL_ALLOW_LOCAL_ARTIFACTS=1 \
    bash "$WORK/candidate/install.sh" >"$WORK/install.out" 2>"$WORK/install.err"
[ ! -s "$WORK/install.out" ]
grep -Fq 'Installed Hamn 0.0.1' "$WORK/install.err"
grep -Fq 'export PATH="$HOME/.local/bin:$PATH"' "$WORK/install.err"
"$HOME_DIR/.local/bin/hamn" --version | grep -Fxq 'hamn 0.0.1'
grep -Fq "\"sha256\":\"$GUEST_HASH\"" \
    "$HOME_DIR/.hamn/cache/guest-image.json"
[ -f "$HOME_DIR/.hamn/cache/hamn-guest-$GUEST_HASH.img" ]
[ -x "$HOME_DIR/.local/share/hamn/src/.hamn-generations"/*/share/hamn/src/scripts/update-host.sh ]
grep -Fq "\"sourceTree\":\"$(git -C "$ROOT" rev-parse HEAD^{tree})\"" \
    "$WORK/candidate/candidate.json"

HOME="$HOME_DIR" PATH="$HOME_DIR/.local/bin:$PATH" HAMN_INSTALL_ALLOW_LOCAL_ARTIFACTS=1 \
    bash "$WORK/candidate/install.sh" >"$WORK/reinstall.out" 2>"$WORK/reinstall.err"
[ ! -s "$WORK/reinstall.out" ]
if grep -Fq 'export PATH=' "$WORK/reinstall.err"; then
    echo "FAIL: bootstrap suggested PATH setup when the command directory was present" >&2
    exit 1
fi
installed_target=$(readlink "$HOME_DIR/.local/bin/hamn")
printf 'tampered\n' >>"$HOST_ARTIFACT"
if HOME="$HOME_DIR" HAMN_INSTALL_ALLOW_LOCAL_ARTIFACTS=1 \
    bash "$WORK/candidate/install.sh" >"$WORK/tampered.out" \
    2>"$WORK/tampered.err"; then
    echo "FAIL: bootstrap accepted a modified host artifact" >&2
    exit 1
fi
grep -Fq 'host artifact SHA-256 mismatch' "$WORK/tampered.err"
[ "$(readlink "$HOME_DIR/.local/bin/hamn")" = "$installed_target" ]

echo "PASS: candidate artifacts bootstrap atomically from immutable release metadata"
