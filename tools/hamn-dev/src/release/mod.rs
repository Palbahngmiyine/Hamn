//! `hamn-dev release SUBCOMMAND ...`: release candidate assembly, evidence
//! contracts, promotion checks, Release Please coordination and the
//! physical validation harness. The shell drivers in packaging/release and
//! the release workflows call these subcommands; none of them is shipped.
//!
//! Drivers named `(env)` below take their inputs from environment variables
//! (documented on each) and run in the checkout of the working directory;
//! see [`checkout`].
pub mod archive;
pub mod candidate;
pub mod checkout;
pub mod contract;
pub mod files;
pub mod github;
pub mod hosted;
pub mod kubernetes;
pub mod physical;
pub mod preflight;
pub mod process;
pub mod runtime;
pub mod syntax;
pub mod version;

use std::path::Path;

type Command = fn(&[String]) -> Result<(), String>;

const COMMANDS: &[(&str, &str, Command)] = &[
    (
        "render-installer",
        "TEMPLATE OUTPUT VERSION COMMIT HOST_URL HOST_SHA256 GUEST_URL GUEST_SHA256 HOST_PATH GUEST_PATH",
        candidate::render_installer,
    ),
    (
        "write-sbom",
        "OUTPUT VERSION COMMIT TREE COMMIT_EPOCH HOST_NAME HOST_SHA256 GUEST_NAME GUEST_SHA256",
        candidate::write_sbom,
    ),
    ("write-candidate", "OUTPUT TAG VERSION COMMIT TREE (NAME SHA256)x4", candidate::write_candidate),
    ("validate-candidate", "DIR TAG COMMIT TREE", validate_candidate),
    ("hosted-evidence", "CANDIDATE_DIR OUTPUT TAG COMMIT TREE RUN ATTEMPT", hosted::hosted_evidence),
    (
        "verify-hosted",
        "CANDIDATE_DIR EVIDENCE STABLE_TAG RC_TAG COMMIT TREE RUN ATTEMPT HOST GUEST SBOM INSTALLER",
        hosted::verify_hosted_command,
    ),
    ("write-manifest", "OUTPUT STABLE_TAG COMMIT BASE_URL CANDIDATE_DIR HOST GUEST", hosted::write_manifest),
    ("verify-draft-release", "RELEASE_JSON TAG COMMIT", hosted::verify_draft_release),
    ("physical-e2e", "[--help]", physical::main),
    ("validate-physical-evidence", "CANDIDATE_JSON SHA256SUMS EVIDENCE RUN ATTEMPT", validate_physical_evidence),
    (
        "external-kubernetes-e2e",
        "--hamn HAMN --context CONTEXT [--kubeconfig PATH] --output PATH [--host-network]",
        kubernetes::main,
    ),
    ("resolve-release", "PREVIOUS_REF (env)", version::resolve_release),
    ("recover-release", "(env)", version::recover_release),
    ("check-version-state", "ROOT", version::check_version_state),
    ("preflight-rulesets", "RULESETS_JSON OUTPUT", preflight::rulesets),
    ("preflight-repository", "REPOSITORY RESPONSES_DIR", preflight::repository),
    ("complete-pr", "TAG COMMIT", github::complete_command),
    ("pr-ready", "", github::ready_command),
];

pub fn run(args: &[String]) -> Result<(), String> {
    let Some((name, rest)) = args.split_first() else {
        return Err(usage());
    };
    match COMMANDS.iter().find(|(command, _, _)| command == name) {
        Some((_, _, command)) => command(rest),
        None => Err(format!("unknown release command {name:?}\n{}", usage())),
    }
}

pub fn usage() -> String {
    let lines: Vec<String> =
        COMMANDS.iter().map(|(name, arguments, _)| format!("  hamn-dev release {name} {arguments}")).collect();
    format!("release commands:\n{}", lines.join("\n"))
}

/// `validate-candidate DIR TAG COMMIT TREE`
fn validate_candidate(args: &[String]) -> Result<(), String> {
    let [directory, tag, commit, tree] = args else {
        return Err("usage: hamn-dev release validate-candidate DIR TAG COMMIT TREE".into());
    };
    contract::validate_candidate(Path::new(directory), tag, commit, tree).map(drop)
}

/// `validate-physical-evidence CANDIDATE_JSON SHA256SUMS EVIDENCE RUN ATTEMPT`
fn validate_physical_evidence(args: &[String]) -> Result<(), String> {
    let [candidate, checksums, evidence, run, attempt] = args else {
        return Err(
            "usage: hamn-dev release validate-physical-evidence CANDIDATE_JSON SHA256SUMS EVIDENCE RUN ATTEMPT".into(),
        );
    };
    contract::validate_physical(Path::new(candidate), Path::new(checksums), Path::new(evidence), run, attempt).map(drop)
}
