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
for release_test in \
    tests/host/test_release_artifacts.sh \
    tests/host/test_release_gate.sh \
    tests/host/test_release_publish.sh; do
    grep -Fq 'unset GITHUB_ACTIONS GITHUB_REPOSITORY GITHUB_RUN_ID GITHUB_RUN_ATTEMPT' \
        "$ROOT/$release_test" ||
        fail "release fixture inherits hosted workflow identity: $release_test"
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
grep -Fq 'environment: hamn-promotion' "$ROOT/.github/workflows/release.yml" ||
    fail "release workflow is missing the protected promotion environment"
if grep -Fq 'environment: hamn-validation' "$ROOT/.github/workflows/release.yml"; then
    fail "hosted-only release still references a physical validation environment"
fi
if awk '/^test-local-macos:/{capture=1; next} capture && /^[^[:space:]]/{exit} capture' \
    "$ROOT/Makefile" | grep -Fq 'test-release-gate'; then
    fail "hosted-only local release gates still require physical VM E2E"
fi
grep -Fqx '  contents: read' "$ROOT/.github/workflows/release.yml" ||
    fail "release workflow must default to read-only repository contents"
grep -Fqx '      contents: write' "$ROOT/.github/workflows/release.yml" ||
    fail "stable promotion must explicitly request repository write access"
if rg -n 'HAMN_(VALIDATOR|RELEASE)_SIGNING_KEY|vars\.HAMN_RELEASE_PUBLIC_KEY|secrets\.HAMN_' \
    "$ROOT/.github/workflows/release.yml" >/dev/null; then
    fail "keyless release workflow still references long-lived signing material"
fi
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
    .["release-type"] == "simple" and
    .["initial-version"] == "0.0.1" and
    .["skip-github-release"] == true and
    .["bump-minor-pre-major"] == true and
    .["bump-patch-for-minor-pre-major"] == true and
    .packages["."]["package-name"] == "hamn"
' "$ROOT/release-please-config.json" >/dev/null ||
    fail "Release Please configuration is invalid"
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
    '    runs-on: macos-15' \
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
    '  workflow_dispatch:' \
    '  contents: read' \
    '    name: Resolve release version' \
    '            bash packaging/release/resolve-release-version.sh "$PREVIOUS_REF"' \
    '            bash packaging/release/resolve-release-request.sh'; do
    grep -Fqx "$requirement" "$release_workflow" ||
        fail "automated release trigger is incomplete: $requirement"
done
if grep -Fq 'commit.verification.verified' "$release_workflow"; then
    fail "release workflow still requires GitHub Verified commits"
fi
if grep -Eq '^[[:space:]]+tags:|rc_run_id:|inputs\.rc_' "$release_workflow"; then
    fail "release workflow exposes an arbitrary tag or cross-run input"
fi
for forbidden in hamn-validator HAMN_VALIDATOR HAMN_RELEASE_SIGNING_KEY \
    'physical validation'; do
    if grep -Fq "$forbidden" "$release_workflow"; then
        fail "hosted-only release retains unsupported authority: $forbidden"
    fi
done
if grep -Eq 'runs-on:[[:space:]]*self-hosted|^[[:space:]]+-[[:space:]]+self-hosted' \
    "$release_workflow"; then
    fail "hosted-only release still schedules a self-hosted runner"
fi

guest_job=$(awk '
    /^  guest-image:$/ { capture = 1 }
    /^  candidate:$/ { capture = 0 }
    capture { print }
' "$release_workflow")
for requirement in \
    '    runs-on: ubuntu-24.04-arm' \
    '    timeout-minutes: 120' \
    '      attestations: write' \
    '      contents: read' \
    '      id-token: write' \
    '            ipxe-qemu jq libguestfs-tools linux-image-virtual passt' \
    '          printf '\''nameserver 169.254.2.2\n'\'' > "$resolver_overlay/etc/resolv.conf"' \
    '            "$guestfs_supermin/zz-hamn-resolver.tar.gz"' \
    '          sudo chmod a+r /boot/vmlinuz-*' \
    '          LIBGUESTFS_BACKEND_SETTINGS=force_tcg \' \
    '            libguestfs-test-tool 2>&1 | tee "$guestfs_test_log"' \
    '          grep -Fq '\''nameserver 169.254.2.2'\'' "$guestfs_test_log"' \
    '          LIBGUESTFS_DEBUG=1 \' \
    '          LIBGUESTFS_TRACE=1 \' \
    '          sudo -u nobody /usr/bin/env -i \' \
    '          builder=$(mktemp -d /tmp/hamn-guest-builder.XXXXXX)' \
    '          git clone --quiet --no-hardlinks --no-checkout \' \
    '          GIT_CONFIG_VALUE_0="$builder/source" \' \
    '          HAMN_GUEST_BASE_IMAGE="$builder/input/base.img" \' \
    '          XDG_RUNTIME_DIR="$builder/runtime" \' \
    '          GIT_CONFIG_KEY_0=safe.directory \' \
    '            /bin/bash "$builder/source/guest/image/build-ubuntu-24.04-arm64.sh"' \
    '          install -m 0644 "$guest_output" "$RUNNER_TEMP/hamn-guest.img"' \
    '      - name: Attest completed guest image'; do
    printf '%s\n' "$guest_job" | grep -Fqx "$requirement" ||
        fail "guest image job is incomplete: $requirement"
done

candidate_job=$(awk '
    /^  candidate:$/ { capture = 1 }
    /^  publish:$/ { capture = 0 }
    capture { print }
' "$release_workflow")
for requirement in \
    '    runs-on: macos-15' \
    '      artifact-metadata: write' \
    '      attestations: write' \
    '      contents: read' \
    '      id-token: write' \
    '          make -j1 SDKROOT="$HAMN_SYSTEM_SDKROOT" test-local-macos' \
    '          make release-hosted-validation \' \
    '      - name: Attest exact candidate artifact provenance' \
    '      - name: Attest hosted validation evidence'; do
    printf '%s\n' "$candidate_job" | grep -Fqx "$requirement" ||
        fail "candidate hosted-validation job is incomplete: $requirement"
done

publish_job=$(awk '
    /^  publish:$/ { capture = 1 }
    capture { print }
' "$release_workflow")
for requirement in \
    '    name: Publish immutable keyless release' \
    '    needs: [prepare, candidate]' \
    '    environment: hamn-promotion' \
    '      attestations: write' \
    '      contents: write' \
    '      id-token: write' \
    '      - name: Verify keyless build provenance' \
    '      - name: Attest immutable update manifest' \
    '      - name: Create and verify draft release' \
    '            --draft --target "$GITHUB_SHA" --title "Hamn $STABLE_TAG" \' \
    '      - name: Publish immutable release and verify assets' \
    '          gh release edit "$STABLE_TAG" --draft=false --latest' \
    '            --json tagName,targetCommitish,isDraft,isPrerelease,isImmutable \' \
    '          gh release verify "$STABLE_TAG"'; do
    printf '%s\n' "$publish_job" | grep -Fqx "$requirement" ||
        fail "keyless immutable promotion is incomplete: $requirement"
done
printf '%s\n' "$publish_job" |
    grep -Fq 'gh attestation verify "$candidate/$name"' ||
    fail "keyless promotion does not verify each candidate attestation"
printf '%s\n' "$publish_job" | grep -Fq -- '--deny-self-hosted-runners' ||
    fail "keyless promotion accepts provenance from self-hosted runners"

for requirement in \
    '"validationMode": "github-hosted-no-vm"' \
    '"physicalE2E": False' \
    '"vmLifecycle": False' \
    '"dockerE2E": False' \
    '"k3sE2E": False'; do
    grep -Fq "$requirement" "$ROOT/packaging/release/hosted-validation.sh" ||
        fail "hosted evidence overstates validation: $requirement"
done
grep -Fq 'BASE_URL="https://github.com/${RELEASE_REPOSITORY}/releases/download/${STABLE_TAG}"' \
    "$ROOT/packaging/release/publish-release.sh" ||
    fail "keyless promotion does not derive the canonical GitHub Release base"
grep -Fq 'candidate artifact directory contains unexpected entries' \
    "$ROOT/packaging/release/publish-release.sh" ||
    fail "keyless promotion does not reject unbound candidate files"
grep -Fq 'CANONICAL_MANIFEST_URL="https://github.com/${RELEASE_REPOSITORY}/releases/latest/download/hamn-update-manifest.json"' \
    "$ROOT/packaging/release/build-candidate.sh" ||
    fail "candidate does not embed the immutable latest manifest URL"
if rg -n 'HAMN_UPDATE_PUBLIC_KEY|hamn-update-manifest\.json\.sig' \
    "$ROOT/packaging/release/build-candidate.sh" \
    "$ROOT/packaging/release/install.sh.in" \
    "$ROOT/packaging/release/publish-release.sh" \
    "$ROOT/scripts/update-host.sh" "$release_workflow" >/dev/null; then
    fail "keyless release path still depends on a long-lived release signature"
fi
if rg -n 'ssh-keygen -Y sign' \
    "$ROOT/packaging/release/build-candidate.sh" \
    "$ROOT/packaging/release/install.sh.in" \
    "$ROOT/packaging/release/publish-release.sh" \
    "$ROOT/scripts/update-host.sh" >/dev/null; then
    fail "keyless release path still signs update metadata with a local key"
fi
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
