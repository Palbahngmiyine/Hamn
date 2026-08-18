#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)

command -v rg >/dev/null 2>&1 || {
    echo "FAIL: legacy runtime removal: rg is required for source-absence checks" >&2
    exit 2
}

fail() {
    printf 'FAIL: legacy runtime removal: %s\n' "$*" >&2
    exit 1
}

assert_untracked() {
    local path=$1
    local tracked

    while IFS= read -r tracked; do
        [ -e "$ROOT/$tracked" ] || continue
        fail "tracked legacy path remains: $path"
    done < <(git -C "$ROOT" ls-files -- "$path" "$path/**")
}

assert_absent() {
    [ ! -e "$ROOT/$1" ] || fail "removed network path remains: $1"
}

for path in \
    desktop \
    host/dockc \
    host/kube \
    host/cmd/cmd_nerdctl.c \
    host/cmd/cmd_kube_connections.c \
    host/core/runtime_status.c \
    host/core/runtime_status.h \
    guest/hamnd \
    guest/systemd/hamn-engine.service \
    guest/scripts/install-runtime.sh \
    guest/tests/test_install_runtime.sh \
    packaging/homebrew \
    packaging/release/colima-benchmark.sh \
    packaging/release/verify-macos-release.sh \
    docs/COLIMA-EVALUATION.md \
    docs/DOGFOODING.md \
    docs/HACKING.md \
    docs/OCI-CONFORMANCE.md \
    docs/RELEASE-CHECKLIST.md \
    docs/RELEASE.md \
    docs/RELEASE.ko.md \
    docs/RELEASE-REVIEW.md \
    docs/RELEASE-REVIEW.ko.md \
    docs/ROADMAP.md \
    packaging/release/README.md \
    tests/ci/test_desktop_xcode.sh \
    tests/e2e/test_installed_cli.sh \
    tests/e2e/test_m4.sh \
    tests/e2e/test_m8.sh \
    tests/host/test_dock_http.c \
    tests/host/test_docker_cli_containers.sh \
    tests/host/test_docker_cli_images.sh \
    tests/host/test_e2e_resource_ownership.sh \
    tests/host/test_exec_plugin_binding.sh \
    tests/host/test_installed_cli_cleanup.sh \
    tests/host/test_kubeconfig_snapshot.sh \
    tests/host/test_kubernetes_connections.sh \
    tests/host/test_legacy_k3s_migration.sh \
    tests/host/test_m8_state_snapshot.sh \
    tests/host/test_managed_kind_cli.sh \
    tests/host/test_nerdctl_cli.sh \
    tests/host/test_profile_state.sh \
    tests/host/test_kubernetes_cli.sh \
    tests/host/fixtures/owned_vmrun.sh \
    tests/host/fixtures/fake_vmrun.c \
    tests/host/fixtures/exec_signal_group.c \
    tests/host/fixtures/tcp_listener_daemon.c \
    tests/host/fixtures/terminal_job_control.c; do
    assert_untracked "$path"
done

for path in \
    guest/agent/ip_reporter.c \
    guest/agent/ip_reporter.h \
    guest/tests/test_ip_reporter.c \
    host/util/guest_ip_report.c \
    host/util/guest_ip_report.h \
    shared/guest_ip_report_protocol.h \
    tests/host/test_guest_ip_report.c; do
    assert_absent "$path"
done

if rg -n \
    'cmd_nerdctl|cmd_kubectl_connection|cmd_kubernetes_connections|managed_kind_cli|legacy_k3s_cli|hamn-engine|HamnDesktop|SMAppService' \
    "$ROOT/host" "$ROOT/Makefile" >/dev/null; then
    fail "host build still refers to a removed public runtime or Kubernetes catalog"
fi

if rg -n '/opt/hamn/src|guest_deployment_sync_sources' \
    "$ROOT/host" "$ROOT/Makefile" >/dev/null; then
    fail "host runtime still deploys mutable guest sources"
fi

if rg -n -- '-I(shared|guest/hamnd)([[:space:]]|$)' \
    "$ROOT/compile_flags.txt" "$ROOT/Makefile" "$ROOT/guest/Makefile" \
    "$ROOT/tests/host/test_port_forwarding.sh" >/dev/null; then
    fail "build configuration still refers to removed source trees"
fi

if rg -n -i 'bridged|network_mode|network_interface|network-mode|network-interface|guest_ip_report|ip_reporter|VZVirtioSocketDeviceConfiguration|com\.apple\.vm\.networking' \
    "$ROOT/host" "$ROOT/Makefile" >/dev/null ||
    rg -n -i 'bridged|network_mode|network_interface|network-mode|network-interface|guest_ip_report|ip_reporter|VZVirtioSocketDeviceConfiguration|com\.apple\.vm\.networking' \
        "$ROOT/guest/agent" "$ROOT/guest/Makefile" >/dev/null; then
    fail "runtime still exposes a non-shared-NAT network selection path"
fi
grep -Fq 'VZNATNetworkDeviceAttachment' "$ROOT/host/vz/vz_config.m" ||
    fail "Virtualization configuration no longer attaches shared NAT"

# mountInotify is an opt-in best-effort bridge, never a persisted no-op
# setting. Keep the macOS watcher, its profile-local agent request, guest
# path-safe timestamp refresh, and physical IN_ATTRIB/IN_CLOSE_WRITE proof coupled.
for requirement in \
    'FSEventStreamCreateFlagFileEvents' \
    'POST /v1/mount-inotify' \
    'ready_write(&watcher->profile, watcher->lease)'; do
    grep -Fq "$requirement" "$ROOT/host/fwd/mount_inotify.c" ||
        fail "mountInotify host bridge is incomplete: $requirement"
done
grep -Fq 'CFLAGS     += -isysroot $(SDKROOT)' "$ROOT/Makefile" ||
    fail "C compilation does not honor the selected system SDK"
grep -Fq 'LDFLAGS    += -isysroot $(SDKROOT)' "$ROOT/Makefile" ||
    fail "linking does not honor the selected system SDK"
for requirement in \
    'mount_inotify_touch(tag->valuestring' \
    'open_existing_regular' \
    'futimens(file, timestamps)'; do
    grep -Fq "$requirement" "$ROOT/guest/agent/api/mount_inotify.c" \
        "$ROOT/guest/agent/api/router.c" ||
        fail "mountInotify guest boundary is incomplete: $requirement"
done
for requirement in \
    'run_hamn configure --mount-inotify true' \
    'IN_ATTRIB | IN_CLOSE_WRITE' \
    'mountInotify did not produce guest IN_ATTRIB and IN_CLOSE_WRITE events'; do
    grep -Fq "$requirement" "$ROOT/packaging/release/physical-e2e.sh" ||
        fail "physical mountInotify proof is incomplete: $requirement"
done
for requirement in \
    'exercise_rosetta()' \
    'run_hamn configure --profile "$profile" --rosetta true' \
    '/mnt/hamn-rosetta/rosetta' \
    '/proc/sys/fs/binfmt_misc/hamn-rosetta' \
    'linux/amd64 container did not execute under Rosetta'; do
    grep -Fq "$requirement" "$ROOT/packaging/release/physical-e2e.sh" ||
        fail "physical Rosetta proof is incomplete: $requirement"
done
for requirement in \
    'environment: hamn-validation' \
    'environment: hamn-promotion'; do
    grep -Fq "$requirement" "$ROOT/.github/workflows/release.yml" ||
        fail "release workflow is missing the required protected environment: $requirement"
done
grep -Fqx '  contents: read' "$ROOT/.github/workflows/release.yml" ||
    fail "release workflow must default to read-only repository contents"
grep -Fqx '      contents: write' "$ROOT/.github/workflows/release.yml" ||
    fail "stable promotion must explicitly request repository write access"
grep -Fqx '          HAMN_RELEASE_PUBLIC_KEY_TEXT: ${{ vars.HAMN_RELEASE_PUBLIC_KEY }}' \
    "$ROOT/.github/workflows/release.yml" ||
    fail "release candidate must receive only the non-secret release public key"
grep -Fqx '          HAMN_VALIDATOR_PUBLIC_KEY_TEXT: ${{ vars.HAMN_VALIDATOR_PUBLIC_KEY }}' \
    "$ROOT/.github/workflows/release.yml" ||
    fail "stable promotion must receive the non-secret validator public key as a variable"
for document in docs/ARCHITECTURE.md docs/ARCHITECTURE.ko.md; do
    if rg -n -i 'non-shared.*(path|virtio|경로)|bounded virtio socket.*guest' \
        "$ROOT/$document" >/dev/null; then
        fail "architecture documentation still describes a removed non-shared path"
    fi
done
grep -Fq 'Every Hamn profile uses Virtualization.framework shared NAT.' \
    "$ROOT/docs/ARCHITECTURE.md" ||
    fail "English architecture documentation does not state shared NAT-only networking"
grep -Fq '모든 Hamn profile은 Virtualization.framework shared NAT를 사용합니다.' \
    "$ROOT/docs/ARCHITECTURE.ko.md" ||
    fail "Korean architecture documentation does not state shared NAT-only networking"

if rg -n 'docker[[:space:]]+login|/home/hamn/\.docker' \
    "$ROOT/host" "$ROOT/guest/scripts" "$ROOT/scripts" >/dev/null; then
    fail "runtime still creates or persists guest registry credentials"
fi

if rg -n --glob '!test_core_quality.sh' \
    'test_install_runtime|install-runtime\.sh' \
    "$ROOT/Makefile" "$ROOT/tests" "$ROOT/scripts" >/dev/null; then
    fail "build or test paths still refer to the removed mutable guest installer"
fi

for nix_file in flake.nix flake.lock; do
    [ -f "$ROOT/$nix_file" ] && [ ! -L "$ROOT/$nix_file" ] ||
        fail "Nix source is missing or unsafe: $nix_file"
done
command -v jq >/dev/null 2>&1 || fail "jq is required to validate flake.lock"
jq -e '
    .version == 7 and .root == "root" and
    .nodes.root.inputs.nixpkgs == "nixpkgs" and
    (.nodes.nixpkgs.locked.rev | test("^[0-9a-f]{40}$")) and
    (.nodes.nixpkgs.locked.narHash | startswith("sha256-"))
' "$ROOT/flake.lock" >/dev/null || fail "flake.lock does not pin nixpkgs"
for requirement in \
    '"aarch64-darwin"' \
    '"x86_64-darwin"' \
    '"aarch64-linux"' \
    '"x86_64-linux"' \
    'devShells = forAllSystems' \
    'release = pkgs.mkShellNoCC' \
    '              kubectl' \
    'checks = forAllSystems' \
    '      actionlintVersion = "1.7.12";' \
    '          ci = pkgs.mkShellNoCC {' \
    'actionlint -config-file'; do
    grep -Fq "$requirement" "$ROOT/flake.nix" ||
        fail "Nix flake is missing required integration: $requirement"
done

jq -e '
    .["."] == "0.0.0"
' "$ROOT/.release-please-manifest.json" >/dev/null ||
    fail "Release Please bootstrap manifest is invalid"
jq -e '
    .["release-type"] == "simple" and
    .["skip-github-release"] == true and
    .["bump-patch-for-minor-pre-major"] == true and
    .packages["."]["package-name"] == "hamn"
' "$ROOT/release-please-config.json" >/dev/null ||
    fail "Release Please configuration is invalid"
grep -Fxq '0.0.1' "$ROOT/version.txt" ||
    fail "initial release version is not 0.0.1"
grep -Fq 'x-release-please-start-version' "$ROOT/Makefile" ||
    fail "Release Please does not update the Makefile version"
grep -Fq 'x-release-please-version' "$ROOT/flake.nix" ||
    fail "Release Please does not update the Nix tooling version"

workflow_files=$(cd "$ROOT" && find .github/workflows -type f -name '*.yml' | LC_ALL=C sort)
expected_workflows=$(printf '%s\n' \
    .github/workflows/ci.yml \
    .github/workflows/release-please.yml \
    .github/workflows/release.yml)
[ "$workflow_files" = "$expected_workflows" ] ||
    fail "workflow set must contain only CI, Release Please, and release"

while IFS= read -r action; do
    [[ "$action" =~ ^[^@]+@[0-9a-f]{40}$ ]] ||
        fail "workflow action is not pinned to a full commit SHA: $action"
done < <(sed -nE 's/^[[:space:]]*uses:[[:space:]]*([^ #]+).*/\1/p' \
    "$ROOT"/.github/workflows/*.yml)

ci_workflow=$ROOT/.github/workflows/ci.yml
for requirement in \
    '  contents: read' \
    '    runs-on: ubuntu-24.04' \
    '    runs-on: macos-14' \
    '        uses: cachix/install-nix-action@13d8dd58da0234aa297dedd986986ccb8e7f3e24 # v31.11.1' \
    '        run: nix flake check --print-build-logs' \
    '        run: nix develop .#ci --command make -j1 test-portable' \
    '      - name: Run macOS regression gates with the system Apple SDK' \
    '          source scripts/ci/use-system-macos-sdk.sh' \
    '          make -j1 SDKROOT="$HAMN_SYSTEM_SDKROOT" test-local-macos'; do
    grep -Fqx "$requirement" "$ci_workflow" ||
        fail "Nix CI workflow is incomplete: $requirement"
done

for requirement in \
    'if [ -n "${HAMN_SYSTEM_SDKROOT:-}" ]; then' \
    '/nix/store/*)' \
    'FAIL: Hamn must not compile against the Nix Apple SDK' \
    'export PATH="/usr/bin:/bin:/usr/sbin:/sbin:$PATH"' \
    '[ "$(command -v clang)" = /usr/bin/clang ]' \
    '[ "$(command -v codesign)" = /usr/bin/codesign ]'; do
    grep -Fq "$requirement" "$ROOT/scripts/ci/use-system-macos-sdk.sh" ||
        fail "system macOS SDK boundary is incomplete: $requirement"
done
for workflow in "$ci_workflow"; do
    grep -Fq 'HAMN_SYSTEM_SDKROOT="$system_sdk" \' "$workflow" ||
        fail "macOS Nix workflow does not pass the pre-resolved system SDK: $workflow"
done

release_please_workflow=$ROOT/.github/workflows/release-please.yml
for requirement in \
    '  contents: read' \
    '        uses: googleapis/release-please-action@45996ed1f6d02564a971a2fa1b5860e934307cf7 # v5.0.0' \
    '      - name: Require dedicated Release Please token' \
    '        run: test -n "$RELEASE_PLEASE_TOKEN"' \
    '          token: ${{ secrets.RELEASE_PLEASE_TOKEN }}' \
    '          skip-github-release: true'; do
    grep -Fqx "$requirement" "$release_please_workflow" ||
        fail "Release Please workflow is incomplete: $requirement"
done

checkout_count=$(grep -hFc 'uses: actions/checkout@' "$ROOT"/.github/workflows/*.yml | \
    awk '{ total += $1 } END { print total + 0 }')
[ "$checkout_count" -gt 0 ] || fail "workflows do not check out source"
[ "$(grep -hFc '          persist-credentials: false' \
    "$ROOT"/.github/workflows/*.yml | awk '{ total += $1 } END { print total + 0 }')" \
    -eq "$checkout_count" ] || fail "every checkout must disable credential persistence"

release_workflow=$ROOT/.github/workflows/release.yml
grep -Fq 'HAMN_SYSTEM_SDKROOT="$system_sdk" \' "$release_workflow" ||
    fail "release workflow does not pass the pre-resolved system SDK"
for requirement in \
    '    branches: [main]' \
    "      - '.release-please-manifest.json'" \
    '  contents: read' \
    '    name: Resolve Release Please version' \
    '        run: bash packaging/release/resolve-release-version.sh "$PREVIOUS_REF"' \
    '      - name: Require GitHub Verified release commit' \
    '          [ "$verified" = true ] || {' \
    '            echo "release commit must be GitHub Verified" >&2'; do
    grep -Fqx "$requirement" "$release_workflow" ||
        fail "automated release trigger is incomplete: $requirement"
done
if grep -Eq 'workflow_dispatch|^[[:space:]]+tags:|rc_run_id:|inputs\.rc_' \
    "$release_workflow"; then
    fail "release workflow still exposes a manual tag or promotion trigger"
fi

candidate_job=$(awk '
    /^  candidate:$/ { capture = 1 }
    /^  validate:$/ { capture = 0 }
    capture { print }
' "$release_workflow")
printf '%s\n' "$candidate_job" | grep -Fqx '    runs-on: macos-14' ||
    fail "candidate job is not pinned to the GitHub-hosted macOS 14 arm64 label"
for permission in \
    '      artifact-metadata: write' \
    '      attestations: write' \
    '      contents: read' \
    '      id-token: write'; do
    printf '%s\n' "$candidate_job" | grep -Fqx "$permission" ||
        fail "candidate job lacks the required attestation permission: $permission"
done
for permission in '      artifact-metadata: write' '      attestations: write' '      id-token: write'; do
    [ "$(grep -Fxc "$permission" "$release_workflow")" -eq 1 ] ||
        fail "only the secret-free candidate job may receive $permission"
done
for requirement in \
    '      - name: Attest exact candidate artifact provenance' \
    '        uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4.2.2' \
    '          subject-checksums: ${{ runner.temp }}/hamn-candidate/SHA256SUMS'; do
    printf '%s\n' "$candidate_job" | grep -Fqx "$requirement" ||
        fail "candidate provenance attestation is incomplete: $requirement"
done
for requirement in \
    '      - name: Assert hosted Apple Silicon arm64 runner' \
    '      - name: Install Nix' \
    '        uses: cachix/install-nix-action@13d8dd58da0234aa297dedd986986ccb8e7f3e24 # v31.11.1' \
    "          nix develop .#ci --command bash -euo pipefail <<'NIX_SHELL'" \
    '          [ "$RUNNER_ENVIRONMENT" = github-hosted ] || {' \
    '          [ "$RUNNER_OS" = macOS ] || {' \
    '          [ "$RUNNER_ARCH" = ARM64 ] || {' \
    '          [ "$(uname -m)" = arm64 ] || {'; do
    printf '%s\n' "$candidate_job" | grep -Fqx "$requirement" ||
        fail "candidate job does not fail closed for its required runner: $requirement"
done
grep -Fq "nix develop .#release --command bash -euo pipefail <<'NIX_SHELL'" \
    "$release_workflow" || fail "physical validation does not use the Nix release shell"
grep -Fqx '          HAMN_VALIDATOR_PATH="$PATH" \' "$release_workflow" ||
    fail "physical validation does not pass the Nix release PATH"
[ "$(grep -Fc "nix develop .#ci --command bash -euo pipefail <<'NIX_SHELL'" \
    "$release_workflow")" -eq 2 ] ||
    fail "candidate build and stable promotion must use the Nix CI shell"
grep -Fqx '          make -j1 SDKROOT="$HAMN_SYSTEM_SDKROOT" test-local-macos' "$release_workflow" ||
    fail "hosted candidate does not rerun the full local regression suite"
grep -Fqx '          source scripts/ci/use-system-macos-sdk.sh' "$release_workflow" ||
    fail "hosted candidate does not use the current system Apple SDK"
if grep -Fq 'HAMN_RELEASE_TEST_FIXTURES' "$release_workflow"; then
    fail "release workflow must not enable fixture validation"
fi
if grep -Fq 'HAMN_GUEST_E2E_COMMAND' \
    "$release_workflow"; then
    fail "release workflow must use candidate-contained physical harnesses"
fi
for requirement in \
    'HAMN_GUEST_E2E_COMMAND is allowed only for test fixtures' \
    'HAMN_RELEASE_ALLOW_DIRTY is allowed only for test fixtures'; do
    grep -Fq "$requirement" "$ROOT/packaging/release/release-gate.sh" ||
        fail "release gate permits a production test-only escape: $requirement"
done
for requirement in \
    'trap cleanup_validator_key EXIT' \
    "trap 'exit 129' HUP" \
    "trap 'exit 130' INT" \
    "trap 'exit 143' TERM" \
    'rm -f -- "$validator_key"'; do
    grep -Fq "$requirement" "$release_workflow" ||
        fail "release validator signing key is not reliably cleaned: $requirement"
done
for requirement in \
    'trap cleanup_publish_keys EXIT' \
    "trap 'exit 129' HUP" \
    "trap 'exit 130' INT" \
    "trap 'exit 143' TERM" \
    'rm -f -- "$validator_public_key" "$release_key"' \
    'validator_public_key="$RUNNER_TEMP/hamn-validator.pub"' \
    'release_key="$RUNNER_TEMP/hamn-release"' \
    'chmod 0600 "$validator_public_key" "$release_key"' \
    'unset HAMN_VALIDATOR_PUBLIC_KEY_TEXT HAMN_RELEASE_SIGNING_KEY_TEXT'; do
    grep -Fq "$requirement" "$release_workflow" ||
        fail "release signing material is not reliably cleaned: $requirement"
done
for requirement in \
    'COLIMA_BINARY_BEFORE_HASH=$(colima_binary_hash)' \
    '"binaryBeforeSha256": binary_before_hash' \
    '"binaryAfterSha256": binary_after_hash' \
    'COLIMA_INSTANCES_BEFORE_HASH=$(colima_instance_inventory_hash)' \
    '"instancesBeforeSha256": instances_before_hash' \
    '"instancesAfterSha256": instances_after_hash'; do
    grep -Fq "$requirement" "$ROOT/packaging/release/physical-e2e.sh" ||
        fail "physical evidence does not bind Colima binary before and after: $requirement"
done
if rg -n 'HAMN_RELEASE_(MANIFEST_URL|BASE_URL):[[:space:]]+\$\{\{[[:space:]]*vars\.' \
    "$release_workflow" >/dev/null; then
    fail "release workflow permits an arbitrary candidate or stable release URL"
fi
grep -Fqx '          HAMN_RELEASE_REPOSITORY: ${{ github.repository }}' \
    "$release_workflow" ||
    fail "stable promotion does not derive its GitHub Release URL from the repository"
grep -Fq 'CANONICAL_MANIFEST_URL="https://github.com/${GITHUB_REPOSITORY}/releases/download/${VERSION}/hamn-update-manifest.json"' \
    "$ROOT/packaging/release/build-candidate.sh" ||
    fail "candidate build does not derive its canonical stable manifest URL"
grep -Fq 'HAMN_RELEASE_MANIFEST_URL must match the canonical GitHub Release manifest URL' \
    "$ROOT/packaging/release/build-candidate.sh" ||
    fail "candidate build does not reject a mismatched GitHub manifest URL"
grep -Fq 'BASE_URL="https://github.com/${RELEASE_REPOSITORY}/releases/download/${STABLE_TAG}"' \
    "$ROOT/packaging/release/publish-release.sh" ||
    fail "stable promotion does not derive its canonical GitHub Release base"
grep -Fq 'candidate update manifest URL does not match the canonical stable release' \
    "$ROOT/packaging/release/publish-release.sh" ||
    fail "stable promotion does not bind the candidate installer to its canonical manifest"
grep -Fq '[ "$(uname -m)" = arm64 ] ||' \
    "$ROOT/packaging/release/build-candidate.sh" ||
    fail "candidate builder does not verify Apple Silicon before labeling an arm64 artifact"
grep -Fq 'release candidate must build on Apple Silicon arm64' \
    "$ROOT/packaging/release/build-candidate.sh" ||
    fail "candidate builder does not reject a non-arm64 host"
grep -Fq 'candidate artifact directory contains unexpected entries' \
    "$ROOT/packaging/release/publish-release.sh" ||
    fail "stable promotion does not reject unbound candidate files"
if grep -Fq 'hamn-"*.tar.gz' "$release_workflow" || \
    grep -Fq 'hamn-"*.img' "$release_workflow" || \
    grep -Fq 'hamn-"*.spdx.json' "$release_workflow"; then
    fail "stable release still uploads wildcard-matched candidate files"
fi
for artifact in \
    'hamn-${STABLE_TAG}-darwin-arm64.tar.gz' \
    'hamn-${STABLE_TAG}-ubuntu-24.04-arm64.img' \
    'hamn-${STABLE_TAG}.spdx.json'; do
    grep -Fq "$artifact" "$release_workflow" ||
        fail "stable release does not upload an exact candidate artifact: $artifact"
done
grep -Fqx '          HAMN_VALIDATOR_SIGNING_KEY_TEXT: ${{ secrets.HAMN_VALIDATOR_SIGNING_KEY }}' \
    "$release_workflow" ||
    fail "physical validator secret is not supplied as key text"
if [ "$(grep -Fc 'secrets.HAMN_VALIDATOR_SIGNING_KEY' "$release_workflow")" -ne 1 ] || \
    [ "$(grep -Fc 'secrets.HAMN_RELEASE_SIGNING_KEY' "$release_workflow")" -ne 1 ]; then
    fail "each private signing key must be visible to exactly one protected job"
fi
if grep -Fq '          HAMN_VALIDATOR_SIGNING_KEY: ${{ secrets.HAMN_VALIDATOR_SIGNING_KEY }}' \
    "$release_workflow"; then
    fail "physical validator passes secret text where a key file path is required"
fi
for requirement in \
    '          umask 077' \
    '          validator_key="$RUNNER_TEMP/hamn-validator-signing-key"' \
    '          printf '\''%s\n'\'' "$HAMN_VALIDATOR_SIGNING_KEY_TEXT" > "$validator_key"' \
    '          chmod 0600 "$validator_key"' \
    '          unset HAMN_VALIDATOR_SIGNING_KEY_TEXT' \
    '          HAMN_VALIDATOR_PATH="$PATH" \' \
    '          HAMN_VALIDATOR_SIGNING_KEY="$validator_key" make release-gate \'; do
    grep -Fqx "$requirement" "$release_workflow" ||
        fail "physical validator key-file preparation is incomplete: $requirement"
done
for requirement in \
    '          HAMN_EXPECTED_WORKFLOW_RUN="$GITHUB_RUN_ID" \' \
    '          HAMN_EXPECTED_WORKFLOW_ATTEMPT="$GITHUB_RUN_ATTEMPT" \'; do
    grep -Fqx "$requirement" "$release_workflow" ||
        fail "automatic promotion does not bind same-run validation evidence: $requirement"
done
for artifact in hamn-candidate hamn-evidence; do
    block=$(awk -v artifact="$artifact" '
        $0 == "          name: " artifact "-${{ github.sha }}" { capture = 1 }
        capture && /^      - name: / { exit }
        capture { print }
    ' "$release_workflow")
    [ -n "$block" ] || fail "same-run $artifact download is missing"
    if printf '%s\n' "$block" | grep -Eq 'github-token:|repository:|run-id:'; then
        fail "same-run $artifact download contains cross-run selectors"
    fi
done
for requirement in \
    '    name: Publish validated release automatically' \
    '    needs: [prepare, candidate, validate]' \
    '      contents: write' \
    '      - name: Create and verify draft release' \
    '            --draft \' \
    '            --target "$GITHUB_SHA" \' \
    '              raise SystemExit("draft release does not bind every validated artifact")' \
    '      - name: Publish immutable release and verify tag' \
    '          gh release edit "$STABLE_TAG" --draft=false --latest' \
    '          [ "$stable_commit" = "$GITHUB_SHA" ] || {'; do
    grep -Fqx "$requirement" "$release_workflow" ||
        fail "automatic stable release is incomplete: $requirement"
done
if rg -n 'git[[:space:]]+tag|git[[:space:]]+push.*refs/tags|--verify-tag' \
    "$release_workflow" >/dev/null; then
    fail "release workflow still depends on a manually prepared tag"
fi
if rg -n -i 'HAMN_COLIMA_BENCHMARK_COMMAND|benchmarkSha256|colima-benchmark' \
    "$ROOT/packaging/release" "$ROOT/Makefile" "$release_workflow" >/dev/null; then
    fail "release path still refers to the removed Colima benchmark gate"
fi

for document in \
    SECURITY.md \
    docs/ARCHITECTURE.md \
    docs/ARCHITECTURE.ko.md \
    docs/CONFIGURATION.md \
    docs/CONFIGURATION.ko.md \
    docs/COLIMA-COMPATIBILITY.md \
    docs/COLIMA-COMPATIBILITY.ko.md \
    docs/DEVELOPMENT.md \
    docs/DEVELOPMENT.ko.md \
    docs/RELEASE-SETUP.md \
    docs/RELEASE-SETUP.ko.md \
    docs/SECURITY.ko.md; do
    [ -f "$ROOT/$document" ] && [ ! -L "$ROOT/$document" ] ||
        fail "required Docker-only documentation is missing: $document"
done

for readme in README.md README.ko.md; do
    grep -Fq 'releases/latest/download/install.sh' "$ROOT/$readme" ||
        fail "README does not direct users to the signed release installer: $readme"
    if rg -n '^[[:space:]]*make (host|install)[[:space:]]*$' "$ROOT/$readme" >/dev/null; then
        fail "README advertises a source installation as an end-user path: $readme"
    fi
done

if git -C "$ROOT" grep -I -n -E \
    -e '-----BEGIN ([A-Z0-9 ]+ )?PRIVATE KEY-----|-----BEGIN OPENSSH PRIVATE KEY-----|-----BEGIN PGP PRIVATE KEY BLOCK-----' \
    -- ':!tests/**' ':!vendor/**' >/dev/null; then
    fail "tracked product files contain private key material"
fi
if git -C "$ROOT" grep -I -n -E \
    -e 'ghp_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|glpat-[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{20,}|sk-[A-Za-z0-9]{20,}' \
    -- ':!vendor/**' >/dev/null; then
    fail "tracked source contains a token-shaped credential"
fi

if rg -n -i \
    'source tree is not a release claim|passing local source tests does not replace|release claim boundary|evidence boundary|source-only test or a fixture' \
    "$ROOT/README.md" "$ROOT/README.ko.md" "$ROOT/docs" >/dev/null; then
    fail "public documentation still contains release-internal evidence messaging"
fi

if rg -n 'containerd\.sock' "$ROOT/host/core/guest_deployment.c" |
    grep -F 'profile_path' >/dev/null; then
    fail "guest containerd socket is still exposed through a host profile"
fi

grep -A1 -F '<key>com.apple.security.virtualization</key>' \
    "$ROOT/host/entitlements.plist" | grep -Fq '<true/>' ||
    fail "required Virtualization entitlement is missing"
"$ROOT/build/hamn" version >/dev/null ||
    fail "the ad-hoc signed Hamn binary is not executable"
help=$("$ROOT/build/hamn" --help)
printf '%s\n' "$help" |
    grep -Fq 'delete   soft-delete the VM; --data removes all profile data' ||
    fail "CLI help does not describe the soft-delete and hard-delete boundary"
if printf '%s\n' "$help" | grep -Fq -- '--force'; then
    fail "CLI help advertises a removed force-delete option"
fi
if printf '%s\n' "$help" | grep -Eq 'internal commands:|^[[:space:]]+(vmrun|qcow2-extract)[[:space:]]'; then
    fail "CLI help exposes internal lifecycle or image commands"
fi
if "$ROOT/build/hamn" delete --force >/dev/null 2>&1; then
    fail "CLI accepted a removed force-delete option"
fi

for private_path in .env .env.local release.pem release.p12 release.pfx \
    release.key id_rsa id_ecdsa id_ed25519; do
    git -C "$ROOT" check-ignore -q "$private_path" ||
        fail "private material is not ignored: $private_path"
done

printf 'PASS: legacy Desktop, Docker API engine, public containerd, and catalog paths are absent\n'
