//! Core quality contracts of the CLI-only product: removed Desktop, Docker
//! API engine, public containerd, Kubernetes catalog, K3s retirement and
//! non-shared-NAT paths stay removed; the flake, workflows and release path
//! keep their pinned, keyless and hosted boundaries; tracked files hold no
//! credentials; and the published executable describes and rejects what it
//! should. Run from the repository root with `HAMN` naming the executable.
use crate::runner::{self, case};
use crate::support::exec::output_within;
use crate::support::{hamn, tmp::TempDir};
use search::{assert_absent, contains, git_grep, lines, listed, read};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode, Output, Stdio};
use std::time::Duration;

mod nix;
mod release;
mod search;
mod workflows;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "core-quality",
        "legacy Desktop, Docker API engine, public containerd, and catalog paths are absent",
        vec![
            case("legacy_paths_stay_untracked", legacy_paths_stay_untracked),
            case("removed_network_and_retirement_paths_are_absent", removed_network_and_retirement_paths_are_absent),
            case("host_build_has_no_removed_runtime_or_catalog", host_build_has_no_removed_runtime_or_catalog),
            case("host_runtime_deploys_no_mutable_guest_sources", host_runtime_deploys_no_mutable_guest_sources),
            case("build_configuration_has_no_removed_source_trees", build_configuration_has_no_removed_source_trees),
            case("networking_is_shared_nat_only", networking_is_shared_nat_only),
            case("mount_inotify_bridge_is_complete", mount_inotify_bridge_is_complete),
            case(
                "release_fixtures_do_not_inherit_workflow_identity",
                release::release_fixtures_do_not_inherit_workflow_identity,
            ),
            case("c_builds_use_the_selected_system_sdk", c_builds_use_the_selected_system_sdk),
            case(
                "physical_release_contract_has_no_legacy_retirement",
                release::physical_release_contract_has_no_legacy_retirement,
            ),
            case(
                "release_workflow_is_keyless_and_least_privilege",
                workflows::release_workflow_is_keyless_and_least_privilege,
            ),
            case("architecture_docs_describe_shared_nat_only", architecture_docs_describe_shared_nat_only),
            case("runtime_persists_no_guest_registry_credentials", runtime_persists_no_guest_registry_credentials),
            case("mutable_guest_installer_stays_removed", mutable_guest_installer_stays_removed),
            case("nix_flake_pins_inputs_and_integrations", nix::nix_flake_pins_inputs_and_integrations),
            case("release_please_configuration_is_valid", nix::release_please_configuration_is_valid),
            case(
                "workflow_set_is_ci_release_please_and_release",
                workflows::workflow_set_is_ci_release_please_and_release,
            ),
            case("workflow_actions_are_pinned_and_allowed", workflows::workflow_actions_are_pinned_and_allowed),
            case("ci_macos_shards_partition_local_gates", workflows::ci_macos_shards_partition_local_gates),
            case("ci_workflow_requires_every_shard", workflows::ci_workflow_requires_every_shard),
            case("ci_workflow_runs_gates_in_nix_shells", workflows::ci_workflow_runs_gates_in_nix_shells),
            case("darwin_shells_own_the_system_sdk_boundary", nix::darwin_shells_own_the_system_sdk_boundary),
            case("workflows_leave_sdk_selection_to_the_flake", workflows::workflows_leave_sdk_selection_to_the_flake),
            case("nix_shell_resolves_the_pinned_toolchain", nix::nix_shell_resolves_the_pinned_toolchain),
            case("release_please_workflow_is_complete", workflows::release_please_workflow_is_complete),
            case("checkouts_do_not_persist_credentials", workflows::checkouts_do_not_persist_credentials),
            case("release_workflow_is_automated_and_hosted", workflows::release_workflow_is_automated_and_hosted),
            case("guest_image_job_is_complete", workflows::guest_image_job_is_complete),
            case("candidate_job_runs_hosted_validation", workflows::candidate_job_runs_hosted_validation),
            case(
                "publish_job_promotes_a_keyless_immutable_release",
                workflows::publish_job_promotes_a_keyless_immutable_release,
            ),
            case(
                "hosted_evidence_does_not_overstate_validation",
                release::hosted_evidence_does_not_overstate_validation,
            ),
            case(
                "keyless_promotion_binds_candidates_and_manifest",
                release::keyless_promotion_binds_candidates_and_manifest,
            ),
            case("release_path_uses_no_long_lived_signature", release::release_path_uses_no_long_lived_signature),
            case("release_workflow_needs_no_manual_tag", workflows::release_workflow_needs_no_manual_tag),
            case("colima_benchmark_gate_stays_removed", release::colima_benchmark_gate_stays_removed),
            case("docker_only_documentation_is_present", docker_only_documentation_is_present),
            case("readmes_direct_users_to_the_release_installer", readmes_direct_users_to_the_release_installer),
            case("tracked_files_hold_no_private_keys_or_tokens", tracked_files_hold_no_private_keys_or_tokens),
            case(
                "public_documentation_has_no_release_internal_messaging",
                public_documentation_has_no_release_internal_messaging,
            ),
            case("guest_containerd_socket_stays_private", guest_containerd_socket_stays_private),
            case("virtualization_entitlement_is_granted", virtualization_entitlement_is_granted),
            case("binary_runs_and_help_describes_data_semantics", binary_runs_and_help_describes_data_semantics),
            case(
                "removed_force_options_are_rejected_before_dispatch",
                removed_force_options_are_rejected_before_dispatch,
            ),
            case("private_material_is_ignored", private_material_is_ignored),
        ],
        filters,
    )
}

/// `[ -f PATH ] && [ ! -L PATH ]`: a regular file, not a symbolic link.
fn is_regular_file(path: &str) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Whether anything, even a dangling symbolic link, is at `path`.
fn present(path: &str) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Removed sources that must not come back as tracked files; an untracked
/// `desktop/` directory can be the user's and is left alone. The host
/// installer and updater are native (`control/install_support`), so no
/// shell installer tree or shell install-test fixture returns.
const UNTRACKED_LEGACY: &[&str] = &[
    "desktop",
    "scripts",
    "tests/host/fixtures",
    "host/dockc",
    "host/kube",
    "host/cmd/cmd_nerdctl.c",
    "host/cmd/cmd_kube_connections.c",
    "host/core/runtime_status.c",
    "host/core/runtime_status.h",
    "guest/hamnd",
    "guest/systemd/hamn-engine.service",
    "guest/scripts/install-runtime.sh",
    "guest/tests/test_install_runtime.sh",
    "packaging/homebrew",
    "packaging/release/colima-benchmark.sh",
    "packaging/release/verify-macos-release.sh",
    "docs/COLIMA-EVALUATION.md",
    "docs/DOGFOODING.md",
    "docs/HACKING.md",
    "docs/OCI-CONFORMANCE.md",
    "docs/RELEASE-CHECKLIST.md",
    "docs/RELEASE.md",
    "docs/RELEASE.ko.md",
    "docs/ROADMAP.md",
    "packaging/release/README.md",
    "tests/ci/test_desktop_xcode.sh",
    "tests/e2e/test_installed_cli.sh",
    "tests/e2e/test_m4.sh",
    "tests/e2e/test_m8.sh",
    "tests/host/test_dock_http.c",
    "tests/host/test_docker_cli_containers.sh",
    "tests/host/test_docker_cli_images.sh",
    "tests/host/test_e2e_resource_ownership.sh",
    "tests/host/test_exec_plugin_binding.sh",
    "tests/host/test_installed_cli_cleanup.sh",
    "tests/host/test_kubeconfig_snapshot.sh",
    "tests/host/test_kubernetes_connections.sh",
    "tests/host/test_legacy_k3s_migration.sh",
    "tests/host/test_m8_state_snapshot.sh",
    "tests/host/test_managed_kind_cli.sh",
    "tests/host/test_nerdctl_cli.sh",
    "tests/host/test_profile_state.sh",
    "tests/host/test_kubernetes_cli.sh",
    "tests/host/fixtures/owned_vmrun.sh",
    "tests/host/fixtures/fake_vmrun.c",
    "tests/host/fixtures/exec_signal_group.c",
    "tests/host/fixtures/tcp_listener_daemon.c",
    "tests/host/fixtures/terminal_job_control.c",
];

/// Removed network selection, guest IP report and K3s retirement sources.
const ABSENT: &[&str] = &[
    "host/core/retirement.c",
    "host/core/retirement.h",
    "host/migration/legacy-k3s.service",
    "host/migration/retire_k3s.py",
    "scripts/embed-retirement.py",
    "tests/host/test_k3s_retirement.py",
    "guest/agent/ip_reporter.c",
    "guest/agent/ip_reporter.h",
    "guest/tests/test_ip_reporter.c",
    "host/util/guest_ip_report.c",
    "host/util/guest_ip_report.h",
    "shared/guest_ip_report_protocol.h",
    "tests/host/test_guest_ip_report.c",
];

/// A path still listed in the index but deleted from the working tree is
/// being removed and does not fail the check.
fn legacy_paths_stay_untracked() {
    for &path in UNTRACKED_LEGACY {
        let remaining: Vec<String> =
            listed(&[], &[path, &format!("{path}/**")]).into_iter().filter(|tracked| present(tracked)).collect();
        assert!(remaining.is_empty(), "tracked legacy path remains: {path} ({})", remaining.join(", "));
    }
}

fn removed_network_and_retirement_paths_are_absent() {
    for path in ABSENT {
        assert!(!present(path), "removed network path remains: {path}");
    }
}

fn host_build_has_no_removed_runtime_or_catalog() {
    assert_absent(
        "cmd_nerdctl|cmd_kubectl_connection|cmd_kubernetes_connections|managed_kind_cli|legacy_k3s|k3s_retirement|retirement_run|hamn_control_migrate|hamn-engine|HamnDesktop|SMAppService",
        &["host", "Makefile", "build.rs", "control"],
        "host build still refers to a removed public runtime or Kubernetes catalog",
    );
}

fn host_runtime_deploys_no_mutable_guest_sources() {
    assert_absent(
        "/opt/hamn/src|guest_deployment_sync_sources",
        &["host", "Makefile"],
        "host runtime still deploys mutable guest sources",
    );
}

fn build_configuration_has_no_removed_source_trees() {
    assert_absent(
        r#"-I(shared|guest/hamnd)([[:space:]"]|$)"#,
        &["compile_flags.txt", "Makefile", "guest/Makefile", "tools/hamn-dev/src/suites/port_forwarding.rs"],
        "build configuration still refers to removed source trees",
    );
}

fn networking_is_shared_nat_only() {
    assert_absent(
        r"(?i)bridged|network_mode|network_interface|network-mode|network-interface|guest_ip_report|ip_reporter|VZVirtioSocketDeviceConfiguration|com\.apple\.vm\.networking",
        &["host", "Makefile", "guest/agent", "guest/Makefile"],
        "runtime still exposes a non-shared-NAT network selection path",
    );
    assert!(
        contains("host/vz/vz_config.m", "VZNATNetworkDeviceAttachment"),
        "Virtualization configuration no longer attaches shared NAT"
    );
}

/// mountInotify is an opt-in best-effort bridge, never a persisted no-op
/// setting. Keep the macOS watcher, its profile-local agent request, guest
/// path-safe timestamp refresh, and physical IN_ATTRIB/IN_CLOSE_WRITE proof
/// coupled.
fn mount_inotify_bridge_is_complete() {
    for requirement in [
        "FSEventStreamCreateFlagFileEvents",
        "POST /v1/mount-inotify",
        "ready_write(&watcher->profile, watcher->lease)",
    ] {
        assert!(
            contains("host/fwd/mount_inotify.c", requirement),
            "mountInotify host bridge is incomplete: {requirement}"
        );
    }
    for requirement in ["mount_inotify_touch(tag->valuestring", "open_existing_regular", "futimens(file, timestamps)"] {
        assert!(
            contains("guest/agent/api/mount_inotify.c", requirement)
                || contains("guest/agent/api/router.c", requirement),
            "mountInotify guest boundary is incomplete: {requirement}"
        );
    }
}

fn c_builds_use_the_selected_system_sdk() {
    assert!(
        contains("Makefile", "CFLAGS     += -isysroot $(SDKROOT)"),
        "C compilation does not honor the selected system SDK"
    );
    assert!(
        contains("Makefile", "LDFLAGS    += -isysroot $(SDKROOT)"),
        "linking does not honor the selected system SDK"
    );
}

fn architecture_docs_describe_shared_nat_only() {
    for document in ["docs/ARCHITECTURE.md", "docs/ARCHITECTURE.ko.md"] {
        assert_absent(
            "(?i)non-shared.*(path|virtio|경로)|bounded virtio socket.*guest",
            &[document],
            "architecture documentation still describes a removed non-shared path",
        );
    }
    assert!(
        contains("docs/ARCHITECTURE.md", "Every Hamn profile uses Virtualization.framework shared NAT."),
        "English architecture documentation does not state shared NAT-only networking"
    );
    assert!(
        contains("docs/ARCHITECTURE.ko.md", "모든 Hamn profile은 Virtualization.framework shared NAT를 사용합니다."),
        "Korean architecture documentation does not state shared NAT-only networking"
    );
}

fn runtime_persists_no_guest_registry_credentials() {
    assert_absent(
        r"docker[[:space:]]+login|/home/hamn/\.docker",
        &["host", "guest/scripts", "control/install_support"],
        "runtime still creates or persists guest registry credentials",
    );
}

/// This suite lives outside the scanned paths, so the scan needs no
/// exclusion for its own pattern.
fn mutable_guest_installer_stays_removed() {
    assert_absent(
        r"test_install_runtime|install-runtime\.sh",
        &["Makefile", "tests", "control/install_support"],
        "build or test paths still refer to the removed mutable guest installer",
    );
}

fn docker_only_documentation_is_present() {
    for document in [
        "SECURITY.md",
        "docs/ARCHITECTURE.md",
        "docs/ARCHITECTURE.ko.md",
        "docs/CONFIGURATION.md",
        "docs/CONFIGURATION.ko.md",
        "docs/COLIMA-COMPATIBILITY.md",
        "docs/COLIMA-COMPATIBILITY.ko.md",
        "docs/DEVELOPMENT.md",
        "docs/DEVELOPMENT.ko.md",
        "docs/RELEASE-SETUP.md",
        "docs/RELEASE-SETUP.ko.md",
        "docs/SECURITY.ko.md",
    ] {
        assert!(is_regular_file(document), "required Docker-only documentation is missing: {document}");
    }
}

fn readmes_direct_users_to_the_release_installer() {
    for readme in ["README.md", "README.ko.md"] {
        assert!(
            contains(readme, "releases/latest/download/install.sh"),
            "README does not direct users to the signed release installer: {readme}"
        );
        assert_absent(
            r"^[[:space:]]*make (host|install)[[:space:]]*$",
            &[readme],
            &format!("README advertises a source installation as an end-user path: {readme}"),
        );
    }
}

// Assembled so that this tracked file holds no private-key marker itself.
const PRIVATE_KEY_MARKERS: &str = concat!(
    "-----BEGIN ([A-Z0-9 ]+ )?PRIVATE KEY-----",
    "|-----BEGIN ",
    "OPENSSH PRIVATE KEY-----",
    "|-----BEGIN ",
    "PGP PRIVATE KEY BLOCK-----",
);
// No text of this pattern is itself token-shaped: a bracket follows each prefix.
const TOKEN_SHAPES: &str = "ghp_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|glpat-[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{20,}|sk-[A-Za-z0-9]{20,}";

/// Test fixtures may hold private keys; no tracked file outside vendored
/// code may hold a token-shaped credential. Hits name only `path:line`.
fn tracked_files_hold_no_private_keys_or_tokens() {
    let keys = git_grep(PRIVATE_KEY_MARKERS, &[":!tests/**", ":!vendor/**"]);
    assert!(keys.is_empty(), "tracked product files contain private key material: {}", keys.join(", "));
    let tokens = git_grep(TOKEN_SHAPES, &[":!vendor/**"]);
    assert!(tokens.is_empty(), "tracked source contains a token-shaped credential: {}", tokens.join(", "));
}

fn public_documentation_has_no_release_internal_messaging() {
    assert_absent(
        "(?i)source tree is not a release claim|passing local source tests does not replace|release claim boundary|evidence boundary|source-only test or a fixture",
        &["README.md", "README.ko.md", "docs"],
        "public documentation still contains release-internal evidence messaging",
    );
}

fn guest_containerd_socket_stays_private() {
    let deployment = read("host/core/guest_deployment.c");
    assert!(
        !lines(&deployment).iter().any(|line| line.contains("containerd.sock") && line.contains("profile_path")),
        "guest containerd socket is still exposed through a host profile"
    );
}

/// `grep -A1 -F KEY | grep -F '<true/>'`: the key's line or the next.
fn virtualization_entitlement_is_granted() {
    let entitlements = read("host/entitlements.plist");
    let lines = lines(&entitlements);
    let granted = (0..lines.len())
        .filter(|&index| lines[index].contains("<key>com.apple.security.virtualization</key>"))
        .any(|index| lines[index..lines.len().min(index + 2)].iter().any(|line| line.contains("<true/>")));
    assert!(granted, "required Virtualization entitlement is missing");
}

/// Runs the executable under test with `home` as HOME and no input.
fn hamn_in(home: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(hamn());
    command.args(arguments).env("HOME", home).stdin(Stdio::null());
    output_within(&mut command, Duration::from_secs(30))
}

fn binary_runs_and_help_describes_data_semantics() {
    let home = TempDir::new("hamn-core-quality-");
    let version = hamn_in(home.path(), &["--version"]);
    assert!(version.status.success(), "the ad-hoc signed Hamn binary is not executable: {version:?}");
    let output = hamn_in(home.path(), &["--help"]);
    assert!(output.status.success(), "hamn --help failed: {output:?}");
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    assert!(
        help.contains("vm delete preserves the VM disk and Docker data."),
        "help does not describe profile deletion semantics"
    );
    assert!(
        help.contains("system uninstall permanently removes all Hamn data."),
        "help does not describe uninstall data deletion"
    );
    assert!(
        !search::any_line_matches(&help, r"internal commands:|^[[:space:]]+(vmrun|qcow2-extract)[[:space:]]"),
        "CLI help exposes internal lifecycle or image commands"
    );
}

fn removed_force_options_are_rejected_before_dispatch() {
    let removed = TempDir::new("hamn-force-delete-");
    assert!(
        !hamn_in(removed.path(), &["delete", "--force"]).status.success(),
        "CLI accepted a removed force-delete option"
    );
    let home = TempDir::new("hamn-force-rejection-");
    let delete = hamn_in(home.path(), &["--headless", "vm", "delete", "--profile", "fixture", "--yes", "--force"]);
    assert!(!delete.status.success(), "CLI accepted upgrade-only force for VM deletion");
    let output = [String::from_utf8_lossy(&delete.stdout), String::from_utf8_lossy(&delete.stderr)].concat();
    assert!(
        output.contains("--check and --force are only supported for system upgrade"),
        "force deletion was not rejected before dispatch: {output}"
    );
    assert!(fs::symlink_metadata(home.path().join(".hamn")).is_err(), "invalid force deletion touched profile state");
}

fn private_material_is_ignored() {
    for private_path in [
        ".env",
        ".env.local",
        "release.pem",
        "release.p12",
        "release.pfx",
        "release.key",
        "id_rsa",
        "id_ecdsa",
        "id_ed25519",
    ] {
        let ignored =
            output_within(Command::new("git").args(["check-ignore", "-q", private_path]), Duration::from_secs(60));
        assert!(ignored.status.success(), "private material is not ignored: {private_path}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn credential_patterns_match_assembled_samples_but_not_their_own_text() {
        let keys = Regex::new(PRIVATE_KEY_MARKERS).unwrap();
        for sample in [
            concat!("-----BEGIN ", "PRIVATE KEY-----"),
            concat!("-----BEGIN ", "RSA PRIVATE KEY-----"),
            concat!("-----BEGIN ", "OPENSSH PRIVATE KEY-----"),
            concat!("-----BEGIN ", "PGP PRIVATE KEY BLOCK-----"),
        ] {
            assert!(keys.is_match(sample), "{sample}");
        }
        assert!(!keys.is_match(concat!("-----BEGIN ", "PUBLIC KEY-----")));
        let tokens = Regex::new(TOKEN_SHAPES).unwrap();
        for sample in [
            concat!("ghp", "_", "abcdefghijklmnopqrstuvwxyz0123456789"),
            concat!("github", "_pat_", "abcdefghij0123456789"),
            concat!("AKIA", "ABCDEFGHIJ012345"),
            concat!("glpat", "-", "abcdefghij0123456789"),
            concat!("xoxb", "-", "abcdefghij0123456789"),
            concat!("sk", "-", "abcdefghij0123456789"),
        ] {
            assert!(tokens.is_match(sample), "{sample}");
        }
        assert!(!tokens.is_match(concat!("ghp", "_", "abcdefghijklmnopqrstuvwxyz012345678")));
        let source = include_str!("mod.rs");
        for line in source.lines() {
            assert!(!keys.is_match(line) && !tokens.is_match(line), "this file matches its own scan: {line}");
        }
    }
}
