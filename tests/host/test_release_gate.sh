#!/bin/bash
# Release evidence must bind exact candidate bytes, the source tree, physical
# harness results, and the validator signature. The fixture overrides only the
# test phase; production uses harnesses unpacked from the exact host candidate.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-gate.XXXXXX)
cleanup() {
    rm -rf "$WORK"
    make -C "$ROOT" host VERSION=0.0.1-dev >/dev/null
}
trap cleanup EXIT

command -v ssh-keygen >/dev/null || {
    echo "SKIP: ssh-keygen is unavailable" >&2
    exit 0
}

REAL_PYTHON3=$(command -v python3 || true)
[ -n "$REAL_PYTHON3" ] || {
    echo "SKIP: python3 is unavailable" >&2
    exit 0
}

# The physical harness must reject the retired docker -> hamn shim before it
# installs or starts a candidate. Exercise only its read-only preflight with a
# complete temporary validator PATH so this test never invokes real Docker or
# Colima state.
PREFLIGHT_HOME=$WORK/preflight-home
PREFLIGHT_BIN=$PREFLIGHT_HOME/.local/bin
mkdir -p "$PREFLIGHT_BIN" "$PREFLIGHT_HOME/.colima"
cat >"$PREFLIGHT_BIN/docker" <<'EOF'
#!/bin/bash
set -eu
case "${1:-}:${2:-}" in
context:--help|compose:version|buildx:version) exit 0 ;;
*) exit 64 ;;
esac
EOF
cat >"$PREFLIGHT_BIN/colima" <<'EOF'
#!/bin/bash
set -eu
[ "${1:-}" = version ]
EOF
cat >"$PREFLIGHT_BIN/python3" <<EOF
#!/bin/bash
exec "$REAL_PYTHON3" "\$@"
EOF
for tool in kubectl curl ssh-keygen tar go node npm java mvn; do
    cat >"$PREFLIGHT_BIN/$tool" <<'EOF'
#!/bin/bash
exit 0
EOF
done
chmod 0755 "$PREFLIGHT_BIN"/*

env -i HOME="$PREFLIGHT_HOME" PATH=/usr/bin:/bin \
    HAMN_RELEASE_TEST_FIXTURES=1 \
    HAMN_TEST_VALIDATOR_PATH="$PREFLIGHT_BIN:/usr/bin:/bin" \
    /bin/bash "$ROOT/packaging/release/physical-e2e.sh" --preflight \
    >"$WORK/preflight-external.out"
grep -Fxq 'physical validator preflight passed without installing or starting Hamn' "$WORK/preflight-external.out" || {
    echo "FAIL: physical preflight rejected a non-Hamn Docker CLI" >&2
    exit 1
}
env -i HOME="$PREFLIGHT_HOME" PATH=/usr/bin:/bin \
    HAMN_VALIDATOR_PATH="$PREFLIGHT_BIN:/usr/bin:/bin" \
    /bin/bash "$ROOT/packaging/release/physical-e2e.sh" --preflight \
    >"$WORK/preflight-nix-path.out"
grep -Fxq 'physical validator preflight passed without installing or starting Hamn' \
    "$WORK/preflight-nix-path.out" || {
    echo "FAIL: physical preflight rejected the explicit Nix validator PATH" >&2
    exit 1
}
if env -i HOME="$PREFLIGHT_HOME" PATH=/usr/bin:/bin \
    HAMN_VALIDATOR_PATH="relative:/usr/bin:/bin" \
    /bin/bash "$ROOT/packaging/release/physical-e2e.sh" --preflight \
    >"$WORK/preflight-relative.out" 2>"$WORK/preflight-relative.err"; then
    echo "FAIL: physical preflight accepted a relative validator PATH" >&2
    exit 1
fi
grep -Fq 'validator PATH contains a relative directory' \
    "$WORK/preflight-relative.err"
if env -i HOME="$PREFLIGHT_HOME" PATH=/usr/bin:/bin \
    HAMN_TEST_VALIDATOR_PATH="$PREFLIGHT_BIN:/usr/bin:/bin" \
    /bin/bash "$ROOT/packaging/release/physical-e2e.sh" --preflight \
    >"$WORK/preflight-path.out" 2>"$WORK/preflight-path.err"; then
    echo "FAIL: physical preflight accepted a production validator PATH override" >&2
    exit 1
fi
grep -Fq 'HAMN_TEST_VALIDATOR_PATH is allowed only for test fixtures' \
    "$WORK/preflight-path.err"

cat >"$PREFLIGHT_BIN/hamn" <<'EOF'
#!/bin/bash
exit 0
EOF
chmod 0755 "$PREFLIGHT_BIN/hamn"
rm "$PREFLIGHT_BIN/docker"
ln -s hamn "$PREFLIGHT_BIN/docker"
if env -i HOME="$PREFLIGHT_HOME" PATH=/usr/bin:/bin \
    HAMN_RELEASE_TEST_FIXTURES=1 \
    HAMN_TEST_VALIDATOR_PATH="$PREFLIGHT_BIN:/usr/bin:/bin" \
    /bin/bash "$ROOT/packaging/release/physical-e2e.sh" --preflight \
    >"$WORK/preflight-shim.out" 2>"$WORK/preflight-shim.err"; then
    echo "FAIL: physical preflight accepted a docker -> hamn shim" >&2
    exit 1
fi
grep -Fq 'required external Docker CLI resolves to Hamn or a legacy docker -> hamn shim' "$WORK/preflight-shim.err" || {
    echo "FAIL: physical preflight did not identify the docker -> hamn shim" >&2
    exit 1
}

if ! python3 - "$ROOT/packaging/release/release-gate.sh" <<'PY'; then
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
if '13.*|14.*|15.*|26.*|27.*' in text or \
        'int(match.group(1)) < 13' not in text:
    raise SystemExit(1)
PY
    echo "FAIL: release gate does not accept all macOS versions 13 and later" >&2
    exit 1
fi

ssh-keygen -q -t ed25519 -N '' -f "$WORK/release-key"
ssh-keygen -q -t ed25519 -N '' -f "$WORK/validator-key"
printf 'guest image fixture\n' >"$WORK/guest.img"
RELEASE_REF=$(git -C "$ROOT" rev-parse HEAD)
RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
OUTPUT_DIR="$WORK/candidate" \
HAMN_GUEST_IMAGE="$WORK/guest.img" \
HAMN_RELEASE_PUBLIC_KEY="$WORK/release-key.pub" \
HAMN_RELEASE_MANIFEST_URL="file://$WORK/manifest.json" \
HAMN_RELEASE_ALLOW_LOCAL=1 \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/build-candidate.sh" >"$WORK/candidate.out"

# Run the candidate-contained physical rollback function without a VM. It
# installs into a nested temporary HOME, then verifies that a signed local
# manifest with a deliberately failing host installer restores both public
# pointers and removes the active journal.
ROLLBACK_WORK=$WORK/physical-update-rollback
ROLLBACK_CANDIDATE=$WORK/candidate
CANDIDATE_HARNESS_DIR=$WORK/candidate-harness
CANDIDATE_HOST_ARCHIVE=$ROLLBACK_CANDIDATE/hamn-v0.0.1-darwin-arm64.tar.gz
CANDIDATE_HOST_ROOT=$CANDIDATE_HARNESS_DIR/hamn-v0.0.1-darwin-arm64
mkdir -m 0700 "$CANDIDATE_HARNESS_DIR"
tar -xzf "$CANDIDATE_HOST_ARCHIVE" -C "$CANDIDATE_HARNESS_DIR"
[ -x "$CANDIDATE_HOST_ROOT/packaging/release/physical-e2e.sh" ] || {
    echo "FAIL: candidate host archive lacks an executable physical E2E harness" >&2
    exit 1
}
PHYSICAL_LIBRARY=$WORK/candidate-physical-e2e-library.sh
awk '/^# physical-e2e main entry point$/ { exit } { print }' \
    "$CANDIDATE_HOST_ROOT/packaging/release/physical-e2e.sh" >"$PHYSICAL_LIBRARY"
DOCKER_CONFIG_WORK=$WORK/docker-config
mkdir -m 0700 "$DOCKER_CONFIG_WORK" "$DOCKER_CONFIG_WORK/validator" \
    "$DOCKER_CONFIG_WORK/validator/.docker" "$DOCKER_CONFIG_WORK/plugins"
cat >"$DOCKER_CONFIG_WORK/validator/.docker/config.json" <<EOF
{"auths":{"registry.example.invalid":{"auth":"must-not-copy"}},"credsStore":"desktop","cliPluginsExtraDirs":["$DOCKER_CONFIG_WORK/plugins"]}
EOF
cat >"$DOCKER_CONFIG_WORK/docker" <<'EOF'
#!/bin/bash
set -euo pipefail
case "${1:-}:${2:-}" in
compose:version|buildx:version) exit 0 ;;
*) exit 64 ;;
esac
EOF
chmod 0755 "$DOCKER_CONFIG_WORK/docker"
if ! (
    source "$PHYSICAL_LIBRARY"
    VALIDATOR_HOME=$DOCKER_CONFIG_WORK/validator
    TEST_HOME=$DOCKER_CONFIG_WORK/test-home
    VALIDATOR_PATH=/usr/bin:/bin
    PYTHON3=$REAL_PYTHON3
    DOCKER=$DOCKER_CONFIG_WORK/docker
    mkdir -m 0700 "$TEST_HOME"
    prepare_docker_cli_config
    "$PYTHON3" - "$TEST_HOME/.docker/config.json" \
        "$DOCKER_CONFIG_WORK/plugins" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    value = json.load(source)
if value != {"cliPluginsExtraDirs": [sys.argv[2]]}:
    raise SystemExit("isolated Docker config retained credentials or lost plugins")
PY
); then
    echo "FAIL: isolated Docker CLI configuration was not sanitized" >&2
    exit 1
fi
COLIMA_INVENTORY_WORK=$WORK/colima-instance-inventory
mkdir -m 0700 "$COLIMA_INVENTORY_WORK"
cat >"$COLIMA_INVENTORY_WORK/colima" <<'EOF'
#!/bin/bash
set -euo pipefail
[ "${1:-}:${2:-}" = 'list:--json' ] || exit 64
[ "${COLIMA_INSTANCE_EMPTY:-0}" != 1 ] || exit 0
if [ "${COLIMA_INSTANCE_REVERSE:-0}" = 1 ]; then
    printf '%s\n' '{"name":"work","status":"Stopped"}' \
        '{"name":"default","status":"Running"}'
else
    printf '%s\n' '{"name":"default","status":"Running"}' \
        '{"name":"work","status":"Stopped"}'
fi
EOF
chmod 0755 "$COLIMA_INVENTORY_WORK/colima"
if ! (
    source "$PHYSICAL_LIBRARY"
    PYTHON3=$REAL_PYTHON3
    COLIMA=$COLIMA_INVENTORY_WORK/colima
    first=$(colima_instance_inventory_hash)
    export COLIMA_INSTANCE_REVERSE=1
    second=$(colima_instance_inventory_hash)
    [ "$first" = "$second" ]
    if COLIMA_INSTANCE_EMPTY=1 colima_instance_inventory_hash \
        >"$COLIMA_INVENTORY_WORK/empty.out" \
        2>"$COLIMA_INVENTORY_WORK/empty.err"; then
        exit 1
    fi
    grep -Fq 'no Colima profiles or VMs were reported' \
        "$COLIMA_INVENTORY_WORK/empty.err"
); then
    echo "FAIL: Colima instance inventory fixture validation failed" >&2
    exit 1
fi

COLIMA_MUTATION_WORK=$WORK/colima-binary-mutation
if (
    source "$PHYSICAL_LIBRARY"
    WORK=$COLIMA_MUTATION_WORK
    mkdir -m 0700 "$WORK"
    VALIDATOR_HOME=$WORK/validator-home
    COLIMA_ROOT=$VALIDATOR_HOME/.colima
    mkdir -p "$COLIMA_ROOT"
    PYTHON3=$REAL_PYTHON3
    COLIMA=$WORK/colima
    cat >"$COLIMA" <<'EOF'
#!/bin/bash
set -euo pipefail
[ "${1:-}:${2:-}" = 'list:--json' ] || exit 64
printf '%s\n' '{"name":"default","status":"Running"}'
EOF
    chmod 0755 "$COLIMA"
    COLIMA_BEFORE_HASH=$(colima_state_hash)
    COLIMA_BINARY_BEFORE_HASH=$(colima_binary_hash)
    COLIMA_INSTANCES_BEFORE_HASH=$(colima_instance_inventory_hash)
    printf 'changed Colima binary fixture\n' >"$COLIMA"
    write_evidence
) >"$WORK/colima-binary.out" 2>"$WORK/colima-binary.err"; then
    echo "FAIL: physical evidence accepted a changed Colima binary" >&2
    exit 1
fi
grep -Fq 'Colima executable changed while testing the isolated Hamn candidate' \
    "$WORK/colima-binary.err"

if ! (
    source "$PHYSICAL_LIBRARY"
    WORK=$ROLLBACK_WORK
    TEST_HOME=$WORK/home
    mkdir -m 0700 "$WORK" "$TEST_HOME"
    VALIDATOR_PATH="/opt/homebrew/bin:/usr/local/bin:$TEST_HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    PYTHON3=$REAL_PYTHON3
    CANDIDATE_VERSION=v0.0.1
    HOST_ARTIFACT=$ROLLBACK_CANDIDATE/hamn-v0.0.1-darwin-arm64.tar.gz
    GUEST_ARTIFACT=$ROLLBACK_CANDIDATE/hamn-v0.0.1-ubuntu-24.04-arm64.img
    HOST_ARTIFACT_HASH=$(sha256_file "$HOST_ARTIFACT")
    GUEST_ARTIFACT_HASH=$(sha256_file "$GUEST_ARTIFACT")
    ARTIFACT_ROOT=$CANDIDATE_HOST_ROOT
    install_candidate
    stage_candidate_guest
    exercise_update_rollback
    grep -Fxq updateRollback "$WORK/tests.txt"
); then
    echo "FAIL: candidate physical signed-update rollback regression failed" >&2
    exit 1
fi

HARNESS=$WORK/harness
mkdir -p "$HARNESS"
cat >"$HARNESS/e2e" <<'EOF'
#!/bin/bash
set -euo pipefail
case "${HAMN_CANDIDATE_DIR:-}" in */candidate) ;; *) exit 2 ;; esac
candidate_hash=$(shasum -a 256 "$HAMN_CANDIDATE_JSON" | awk '{print $1}')
host_hash=$(awk '$2 ~ /darwin-arm64.tar.gz$/ { print $1 }' "$HAMN_CANDIDATE_DIR/SHA256SUMS")
guest_hash=$(awk '$2 ~ /ubuntu-24.04-arm64.img$/ { print $1 }' "$HAMN_CANDIDATE_DIR/SHA256SUMS")
printf '%s\n' '{"schemaVersion":1,"kind":"hamn-physical-e2e","passed":true,"tests":{"lifecycle":true,"multiProfile":true,"staleSocket":true,"dockerContextRestore":true,"mount":true,"network":true,"tcpPort":true,"udpPort":true,"amd64":true,"rosetta":true,"dockerCli":true,"compose":true,"buildx":true,"dockerSdkGo":true,"dockerSdkPython":true,"testcontainersJava":true,"testcontainersGo":true,"testcontainersPython":true,"testcontainersNode":true,"k3s":true,"updateRollback":true,"softDeleteRecovery":true,"hardDelete":true,"uninstall":true,"colimaCoexistence":true},"provenance":{"candidateJsonSha256":"'"$candidate_hash"'","hostArtifactSha256":"'"$host_hash"'","guestImageSha256":"'"$guest_hash"'"},"colima":{"beforeSha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","afterSha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","binaryBeforeSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","binaryAfterSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","instancesBeforeSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","instancesAfterSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}' >"$HAMN_E2E_OUTPUT"
EOF
chmod 0755 "$HARNESS/e2e"

cp "$HARNESS/e2e" "$HARNESS/e2e-colima-changed"
perl -0pi -e 's/"afterSha256":"c{64}"/q{"afterSha256":"} . ("d" x 64) . q{"}/e' \
    "$HARNESS/e2e-colima-changed"
chmod 0755 "$HARNESS/e2e-colima-changed"
cp "$HARNESS/e2e" "$HARNESS/e2e-colima-binary-changed"
perl -0pi -e 's/"binaryAfterSha256":"b{64}"/q{"binaryAfterSha256":"} . ("d" x 64) . q{"}/e' \
    "$HARNESS/e2e-colima-binary-changed"
chmod 0755 "$HARNESS/e2e-colima-binary-changed"
cp "$HARNESS/e2e" "$HARNESS/e2e-colima-instances-changed"
perl -0pi -e 's/"instancesAfterSha256":"a{64}"/q{"instancesAfterSha256":"} . ("d" x 64) . q{"}/e' \
    "$HARNESS/e2e-colima-instances-changed"
chmod 0755 "$HARNESS/e2e-colima-instances-changed"
if RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
CANDIDATE_DIR="$WORK/candidate" \
OUTPUT_DIR="$WORK/override-rejected" \
HAMN_VALIDATOR_SIGNING_KEY="$WORK/validator-key" \
HAMN_VALIDATOR_IDENTITY=test-validator \
HAMN_GUEST_E2E_COMMAND="$HARNESS/e2e" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/release-gate.sh" >"$WORK/override.out" \
    2>"$WORK/override.err"; then
    echo "FAIL: release gate accepted a non-candidate E2E override" >&2
    exit 1
fi
grep -Fq 'HAMN_GUEST_E2E_COMMAND is allowed only for test fixtures' \
    "$WORK/override.err"

if RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
CANDIDATE_DIR="$WORK/candidate" \
OUTPUT_DIR="$WORK/dirty-rejected" \
HAMN_VALIDATOR_SIGNING_KEY="$WORK/validator-key" \
HAMN_VALIDATOR_IDENTITY=test-validator \
HAMN_RELEASE_ALLOW_DIRTY=1 \
    bash "$ROOT/packaging/release/release-gate.sh" >"$WORK/dirty.out" \
    2>"$WORK/dirty.err"; then
    echo "FAIL: release gate accepted a production dirty-tree bypass" >&2
    exit 1
fi
grep -Fq 'HAMN_RELEASE_ALLOW_DIRTY is allowed only for test fixtures' \
    "$WORK/dirty.err"

if RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
CANDIDATE_DIR="$WORK/candidate" \
OUTPUT_DIR="$WORK/colima-rejected" \
HAMN_VALIDATOR_SIGNING_KEY="$WORK/validator-key" \
HAMN_VALIDATOR_IDENTITY=test-validator \
HAMN_GUEST_E2E_COMMAND="$HARNESS/e2e-colima-changed" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
HAMN_RELEASE_TEST_FIXTURES=1 \
    bash "$ROOT/packaging/release/release-gate.sh" >"$WORK/colima.out" \
    2>"$WORK/colima.err"; then
    echo "FAIL: release gate accepted changed Colima state evidence" >&2
    exit 1
fi
grep -Fq 'Colima coexistence evidence is invalid or changed' "$WORK/colima.err"

if RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
CANDIDATE_DIR="$WORK/candidate" \
OUTPUT_DIR="$WORK/colima-binary-rejected" \
HAMN_VALIDATOR_SIGNING_KEY="$WORK/validator-key" \
HAMN_VALIDATOR_IDENTITY=test-validator \
HAMN_GUEST_E2E_COMMAND="$HARNESS/e2e-colima-binary-changed" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
HAMN_RELEASE_TEST_FIXTURES=1 \
    bash "$ROOT/packaging/release/release-gate.sh" >"$WORK/colima-binary.out" \
    2>"$WORK/colima-binary.err"; then
    echo "FAIL: release gate accepted changed Colima binary evidence" >&2
    exit 1
fi
grep -Fq 'Colima coexistence evidence is invalid or changed' \
    "$WORK/colima-binary.err"

if RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
CANDIDATE_DIR="$WORK/candidate" \
OUTPUT_DIR="$WORK/colima-instances-rejected" \
HAMN_VALIDATOR_SIGNING_KEY="$WORK/validator-key" \
HAMN_VALIDATOR_IDENTITY=test-validator \
HAMN_GUEST_E2E_COMMAND="$HARNESS/e2e-colima-instances-changed" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
HAMN_RELEASE_TEST_FIXTURES=1 \
    bash "$ROOT/packaging/release/release-gate.sh" >"$WORK/colima-instances.out" \
    2>"$WORK/colima-instances.err"; then
    echo "FAIL: release gate accepted changed Colima instance evidence" >&2
    exit 1
fi
grep -Fq 'Colima coexistence evidence is invalid or changed' \
    "$WORK/colima-instances.err"

RELEASE_REF="$RELEASE_REF" \
RELEASE_TAG=v0.0.1-rc.1 \
CANDIDATE_DIR="$WORK/candidate" \
OUTPUT_DIR="$WORK/evidence" \
HAMN_VALIDATOR_SIGNING_KEY="$WORK/validator-key" \
HAMN_VALIDATOR_IDENTITY=test-validator \
HAMN_GUEST_E2E_COMMAND="$HARNESS/e2e" \
HAMN_RELEASE_ALLOW_DIRTY=1 \
HAMN_RELEASE_TEST_FIXTURES=1 \
    bash "$ROOT/packaging/release/release-gate.sh" >"$WORK/gate.out"

[ -f "$WORK/evidence/validation-evidence.json" ]
[ -f "$WORK/evidence/validation-evidence.json.sig" ]
grep -Fq '"identity":"test-validator"' "$WORK/evidence/validation-evidence.json"
grep -Fq '"harnesses":{"e2eSha256":' "$WORK/evidence/validation-evidence.json"
grep -Fq '"artifacts":{"hamn-v0.0.1-darwin-arm64.tar.gz":' \
    "$WORK/evidence/validation-evidence.json"
[ ! -e "$WORK/evidence/benchmark.json" ]
allowed=$WORK/allowed-signers
printf 'hamn-validator ' >"$allowed"
cat "$WORK/validator-key.pub" >>"$allowed"
ssh-keygen -Y verify -f "$allowed" -I hamn-validator -n hamn-validator \
    -s "$WORK/evidence/validation-evidence.json.sig" \
    <"$WORK/evidence/validation-evidence.json" >/dev/null

echo "PASS: physical release gate evidence binds exact candidate bytes"
