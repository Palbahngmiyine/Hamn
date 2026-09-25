//! Release path contracts outside the workflow files: release fixtures run
//! without the hosted workflow's identity, the physical and hosted evidence
//! claim no more than they exercise, and promotion is keyless and binds the
//! exact candidate and the v3 manifest.
use super::search::{assert_absent, contains};

const HOSTED_EVIDENCE: &str = "tools/hamn-dev/src/release/hosted.rs";
const PUBLISH: &str = "tools/hamn-dev/src/release/publish.rs";
const CANDIDATE: &str = "tools/hamn-dev/src/release/candidate.rs";
const INSTALLER: &str = "packaging/release/install.sh.in";
const UPDATER: &str = "control/install_support/update.rs";
const RELEASE_ARTIFACTS: &str = "tools/hamn-dev/src/suites/release_artifacts.rs";
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";
/// Release driver fixtures shared by the Rust release suites.
const RELEASE_DRIVER: &str = "tools/hamn-dev/src/support/release_driver.rs";

pub fn release_fixtures_do_not_inherit_workflow_identity() {
    assert!(
        contains(
            RELEASE_ARTIFACTS,
            r#"for name in ["GITHUB_ACTIONS", "GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"] {"#
        ) && contains(RELEASE_ARTIFACTS, "command.env_remove(name);"),
        "release fixture inherits hosted workflow identity: {RELEASE_ARTIFACTS}"
    );
    assert!(
        contains(
            RELEASE_DRIVER,
            r#"pub const WORKFLOW_IDENTITY: [&str; 4] = ["GITHUB_ACTIONS", "GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"];"#
        ) && contains(RELEASE_DRIVER, "    for name in WORKFLOW_IDENTITY {\n        command.env_remove(name);"),
        "release driver fixtures do not drop the hosted workflow identity"
    );
    // Rust release suites start this executable only through
    // release_command (identity dropped) or hamn_dev (empty environment).
    for release_suite in
        ["tools/hamn-dev/src/suites/hosted_validation.rs", "tools/hamn-dev/src/suites/release_publish.rs"]
    {
        assert!(
            contains(release_suite, "release_command(") && !contains(release_suite, "current_exe()"),
            "release fixture inherits hosted workflow identity: {release_suite}"
        );
    }
}

pub fn physical_release_contract_has_no_legacy_retirement() {
    let contract = "tools/hamn-dev/src/release/contract.rs";
    for requirement in [
        r#""dockerWithoutCli""#,
        r#""vmSurvivesTuiExit""#,
        r#""externalKubernetes""#,
        r#""kubeconfigUnchanged""#,
        "pub const PHYSICAL_SCHEMA_VERSION: u64 = 3;",
    ] {
        assert!(contains(contract, requirement), "physical release contract is incomplete: {requirement}");
    }
    assert_absent(
        "k3sRunningRetirement|k3sStoppedRetirement|dockerDataPreserved|HAMN_LEGACY_",
        &["tools/hamn-dev/src/release", "packaging/release"],
        "physical release gate still requires legacy K3s retirement",
    );
}

pub fn hosted_evidence_does_not_overstate_validation() {
    for requirement in [
        r#""validationMode": "github-hosted-no-vm""#,
        r#""physicalE2E": false"#,
        r#"pub const HOSTED_NOT_EXERCISED: [&str; 3] = ["vmLifecycle", "dockerE2E", "colimaCoexistence"];"#,
    ] {
        assert!(contains(HOSTED_EVIDENCE, requirement), "hosted evidence overstates validation: {requirement}");
    }
    assert!(
        contains(HOSTED_EVIDENCE, "let evidence = evidence_for(candidate_dir, &tag, &commit, &tree, &run, &attempt)?;"),
        "hosted validation does not write its evidence with the checked writer"
    );
    // hosted.rs's own test asserts the written evidence has no k3sE2E check.
    assert_absent("k3sE2E", &["packaging/release"], "hosted evidence still records the removed K3s capability");
}

pub fn keyless_promotion_binds_candidates_and_manifest() {
    assert!(
        contains(PUBLISH, r#"format!("https://github.com/{repository}/releases/download/{stable_tag}")"#),
        "keyless promotion does not derive the canonical GitHub Release base"
    );
    assert!(
        contains(HOSTED_EVIDENCE, "candidate artifact directory contains unexpected entries")
            && contains(PUBLISH, "verify_hosted(&candidate_dir, &evidence, &promotion)?;"),
        "keyless promotion does not reject unbound candidate files"
    );
    assert!(
        contains(
            CANDIDATE,
            r#"format!("https://github.com/{repository}/releases/latest/download/hamn-update-manifest-v3.json")"#
        ),
        "candidate does not embed the latest v3 manifest URL"
    );
}

pub fn release_path_uses_no_long_lived_signature() {
    assert_absent(
        r"HAMN_UPDATE_PUBLIC_KEY|hamn-update-manifest\.json\.sig",
        &[CANDIDATE, INSTALLER, PUBLISH, UPDATER, RELEASE_WORKFLOW],
        "keyless release path still depends on a long-lived release signature",
    );
    assert_absent(
        "ssh-keygen -Y sign",
        &[CANDIDATE, INSTALLER, PUBLISH, UPDATER],
        "keyless release path still signs update metadata with a local key",
    );
}

pub fn colima_benchmark_gate_stays_removed() {
    assert_absent(
        r"(?i)HAMN_COLIMA_BENCHMARK_COMMAND|benchmarkSha256|colima-benchmark",
        &["packaging/release", "Makefile", RELEASE_WORKFLOW],
        "release path still refers to the removed Colima benchmark gate",
    );
}
