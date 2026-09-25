//! `release gate` admits only exact candidate bytes on a physical Apple
//! Silicon validator: every input, the clean checkout at the candidate's
//! commit, the candidate contract and an empty, unlinked output directory
//! are checked before the harness runs. Real VM checks run only in
//! `make release-gate`; here the harness is reached with no Docker CLI or
//! kubectl on PATH and must stop before writing evidence.
use crate::runner::{self, case};
use crate::support::release_driver::{
    Outcome, Repo, candidate_directory, hamn_dev, is_empty_directory, pairs, private_bin, text, with,
};
use std::fs;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-gate",
        "physical gate inputs, source identity and output safety",
        vec![
            case("missing_inputs_are_rejected_before_any_effect", missing_inputs_are_rejected_before_any_effect),
            case("harness_inputs_are_checked_before_any_effect", harness_inputs_are_checked_before_any_effect),
            case("only_an_arm64_validator_is_accepted", only_an_arm64_validator_is_accepted),
            case("dirty_checkout_is_rejected", dirty_checkout_is_rejected),
            case("release_ref_must_be_the_checked_out_commit", release_ref_must_be_the_checked_out_commit),
            case(
                "candidate_must_be_a_real_directory_for_this_source",
                candidate_must_be_a_real_directory_for_this_source,
            ),
            case("output_directory_must_be_empty_and_unlinked", output_directory_must_be_empty_and_unlinked),
            case("valid_inputs_reach_the_harness_without_evidence", valid_inputs_reach_the_harness_without_evidence),
        ],
        filters,
    )
}

const TAG: &str = "v1.0.0-rc.1";

/// A clean two-commit checkout, a candidate for its HEAD and the gate's
/// environment: a PATH with git and a `uname` fixture only.
struct Gate {
    repo: Repo,
    parent: String,
    environment: Vec<(String, String)>,
}

impl Gate {
    fn new() -> Self {
        let repo = Repo::new("hamn-release-gate-");
        repo.write("README", "first\n");
        let parent = repo.commit_all("first");
        repo.write("README", "second\n");
        let commit = repo.commit_all("second");
        candidate_directory(&repo.scratch("candidate"), TAG, &commit, &repo.tree());
        let bin = private_bin(&repo.scratch(""), &["git"], &["uname"]);
        let environment = vec![
            ("PATH".into(), text(&bin)),
            ("HAMN_DEV_FIXTURE".into(), "release-uname".into()),
            ("RELEASE_REF".into(), commit),
            ("RELEASE_TAG".into(), TAG.into()),
            ("CANDIDATE_DIR".into(), text(&repo.scratch("candidate"))),
            ("OUTPUT_DIR".into(), text(&repo.scratch("evidence/physical"))),
            ("HAMN_E2E_CONTEXT".into(), "hamn-test-context".into()),
            ("HAMN_E2E_KUBECONFIG".into(), text(&repo.scratch("kubeconfig"))),
        ];
        Self { repo, parent, environment }
    }

    fn run(&self, environment: &[(String, String)]) -> Outcome {
        hamn_dev(&["release", "gate"], self.repo.root(), &pairs(environment))
    }

    /// The gate fails with `message` and creates no output directory.
    fn rejected(&self, environment: &[(String, String)], message: &str) {
        self.run(environment).failed_with(&format!("release gate: {message}"));
        assert!(!self.repo.scratch("evidence").exists(), "{message}: the output directory was created");
    }

    fn changed(&self, name: &str, value: Option<&str>) -> Vec<(String, String)> {
        with(&self.environment, name, value)
    }
}

fn missing_inputs_are_rejected_before_any_effect() {
    let gate = Gate::new();
    for name in ["RELEASE_REF", "RELEASE_TAG", "CANDIDATE_DIR", "OUTPUT_DIR"] {
        for value in [None, Some("")] {
            gate.rejected(
                &gate.changed(name, value),
                "RELEASE_REF, RELEASE_TAG, CANDIDATE_DIR, and OUTPUT_DIR are required",
            );
        }
    }
}

fn harness_inputs_are_checked_before_any_effect() {
    let gate = Gate::new();
    gate.rejected(&gate.changed("HAMN_E2E_CONTEXT", None), "missing physical validator inputs: HAMN_E2E_CONTEXT");
    gate.rejected(&gate.changed("HAMN_E2E_KUBECONFIG", None), "missing physical validator inputs: HAMN_E2E_KUBECONFIG");
    gate.rejected(&gate.changed("HAMN_E2E_K8S_HOST_NETWORK", Some("yes")), "HAMN_E2E_K8S_HOST_NETWORK must be 0 or 1");
}

fn only_an_arm64_validator_is_accepted() {
    let gate = Gate::new();
    for machine in ["x86_64", "arm64e", ""] {
        gate.rejected(&gate.changed("HAMN_TEST_MACHINE", Some(machine)), "physical Apple Silicon validator required");
    }
}

fn dirty_checkout_is_rejected() {
    let gate = Gate::new();
    gate.repo.write("untracked", "x\n");
    gate.rejected(&gate.environment, "validator source tree is dirty");
    fs::remove_file(gate.repo.root().join("untracked")).unwrap();
    gate.repo.write("README", "modified\n");
    gate.rejected(&gate.environment, "validator source tree is dirty");
}

fn release_ref_must_be_the_checked_out_commit() {
    let gate = Gate::new();
    gate.rejected(&gate.changed("RELEASE_REF", Some(&gate.parent)), "release commit differs from checkout");
    gate.rejected(&gate.changed("RELEASE_REF", Some("no-such-ref")), "RELEASE_REF is not a commit");
    // A symbolic name of the checked-out commit is that commit.
    let outcome = gate.run(&gate.changed("RELEASE_REF", Some("HEAD")));
    outcome.failed_with("external Docker CLI and kubectl fixture tools are required");
}

fn candidate_must_be_a_real_directory_for_this_source() {
    let gate = Gate::new();
    let candidate = gate.repo.scratch("candidate");
    let link = gate.repo.scratch("candidate-link");
    std::os::unix::fs::symlink(&candidate, &link).unwrap();
    let file = gate.repo.scratch("candidate-file");
    fs::write(&file, "").unwrap();
    for path in [link, file, gate.repo.scratch("missing")] {
        gate.rejected(&gate.changed("CANDIDATE_DIR", Some(&text(&path))), "CANDIDATE_DIR is unsafe");
    }
    gate.rejected(&gate.changed("RELEASE_TAG", Some("v1.0.0-rc.2")), "candidate source identity mismatch");
    let other = gate.repo.scratch("other-source");
    candidate_directory(&other, TAG, &gate.parent, &gate.repo.tree());
    gate.rejected(&gate.changed("CANDIDATE_DIR", Some(&text(&other))), "candidate source identity mismatch");
    fs::write(candidate.join("install.sh"), "tampered\n").unwrap();
    gate.rejected(&gate.environment, "candidate artifact digest mismatch");
}

fn output_directory_must_be_empty_and_unlinked() {
    let gate = Gate::new();
    let output = gate.repo.scratch("evidence/physical");
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("previous-evidence.json"), "{}").unwrap();
    gate.run(&gate.environment).failed_with("release gate: OUTPUT_DIR must be empty");
    assert_eq!(fs::read_to_string(output.join("previous-evidence.json")).unwrap(), "{}");
    let empty = gate.repo.scratch("empty");
    fs::create_dir(&empty).unwrap();
    let link = gate.repo.scratch("output-link");
    std::os::unix::fs::symlink(&empty, &link).unwrap();
    gate.run(&gate.changed("OUTPUT_DIR", Some(&text(&link)))).failed_with("release gate: OUTPUT_DIR is unsafe");
    assert!(is_empty_directory(&empty));
}

fn valid_inputs_reach_the_harness_without_evidence() {
    let gate = Gate::new();
    // Every gate check passed: the harness itself stops at its tools.
    let outcome = gate.run(&gate.environment);
    outcome.failed_with("release gate: external Docker CLI and kubectl fixture tools are required");
    assert!(!outcome.stdout.contains("validated exact candidate"), "{outcome:#?}");
    // OUTPUT_DIR was created with its parents and holds no evidence.
    assert!(is_empty_directory(&gate.repo.scratch("evidence/physical")));
}
