//! `release hosted-validation` binds a hosted regression run to exact
//! candidate bytes without claiming a VM, Docker, Colima or physical run.
//!
//! The first case builds a real candidate from this checkout with
//! `release build-candidate` (which rebuilds build/hamn as v0.0.1 and is
//! restored afterwards), as the release workflow does; run from the
//! repository root. The other cases use disposable checkouts and
//! candidate fixtures.
use crate::release::files::sha256_file;
use crate::runner::{self, case};
use crate::support::exec::output_within;
use crate::support::release_driver::{
    Outcome, Repo, RestoreHost, candidate_directory, git, hamn_dev, is_empty_directory, outcome, pairs,
    release_command, text, with,
};
use crate::support::tmp::TempDir;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "hosted-validation",
        "hosted validation binds exact bytes without claiming physical E2E",
        vec![
            case("built_candidate_binds_exact_bytes", built_candidate_binds_exact_bytes),
            case("local_run_evidence_binds_the_candidate", local_run_evidence_binds_the_candidate),
            case("workflow_identity_must_be_decimal_or_local", workflow_identity_must_be_decimal_or_local),
            case("inputs_and_candidate_tag_are_required", inputs_and_candidate_tag_are_required),
            case("release_ref_must_be_the_checked_out_commit", release_ref_must_be_the_checked_out_commit),
            case("directories_must_be_real_and_output_empty", directories_must_be_real_and_output_empty),
            case("candidate_metadata_must_be_regular_files", candidate_metadata_must_be_regular_files),
            case("modified_or_foreign_candidates_are_rejected", modified_or_foreign_candidates_are_rejected),
        ],
        filters,
    )
}

const RC: &str = "v0.0.1-rc.417123456";

/// The capability claims hosted evidence must make, exactly.
fn hosted_checks() -> Value {
    json!({"testLocalMacOS": true, "artifactHashes": true, "archiveSafety": true, "guestImageContract": true,
        "vmLifecycle": false, "dockerE2E": false, "colimaCoexistence": false})
}

fn built_candidate_binds_exact_bytes() {
    let _restore = RestoreHost::capture();
    let work = TempDir::new("hamn-hosted-validation-");
    let guest = work.path().join("guest.img");
    fs::write(&guest, "immutable guest image fixture\n").unwrap();
    let candidate = text(&work.path().join("candidate"));
    let release_ref = git(Path::new("."), &["rev-parse", "HEAD"]);
    let tree = git(Path::new("."), &["rev-parse", "HEAD^{tree}"]);
    let mut build = release_command(&[
        ("GITHUB_REPOSITORY", "example/hamn"),
        ("RELEASE_REF", &release_ref),
        ("RELEASE_TAG", RC),
        ("OUTPUT_DIR", &candidate),
        ("HAMN_GUEST_IMAGE", &text(&guest)),
        ("HAMN_RELEASE_ALLOW_DIRTY", "1"),
    ]);
    build.args(["release", "build-candidate"]);
    outcome(output_within(&mut build, Duration::from_secs(3600))).succeeded();

    let evidence = work.path().join("evidence");
    let validate = |output: &Path| {
        let mut command = release_command(&[
            ("RELEASE_REF", &release_ref),
            ("RELEASE_TAG", RC),
            ("CANDIDATE_DIR", &candidate),
            ("OUTPUT_DIR", &text(output)),
            ("GITHUB_RUN_ID", "417123456"),
            ("GITHUB_RUN_ATTEMPT", "2"),
        ]);
        command.args(["release", "hosted-validation"]);
        outcome(output_within(&mut command, Duration::from_secs(300)))
    };
    validate(&evidence).succeeded();
    let value: Value =
        serde_json::from_slice(&fs::read(evidence.join("hosted-validation-evidence.json")).unwrap()).unwrap();
    assert_eq!(value["kind"], "hamn-hosted-validation-evidence");
    assert_eq!(
        (value["validationMode"].clone(), value["physicalE2E"].clone()),
        (json!("github-hosted-no-vm"), json!(false))
    );
    assert_eq!((value["commit"].as_str(), value["sourceTree"].as_str()), (Some(&release_ref[..]), Some(&tree[..])));
    assert_eq!(value["workflow"], json!({"run": "417123456", "attempt": "2"}));
    assert_eq!(value["checks"], hosted_checks(), "hosted evidence claims more or less than it ran");

    let host = Path::new(&candidate).join("hamn-v0.0.1-darwin-arm64.tar.gz");
    let mut bytes = fs::read(&host).unwrap();
    bytes.extend_from_slice(b"tampered\n");
    fs::write(&host, bytes).unwrap();
    validate(&work.path().join("tampered-evidence")).failed_with("candidate artifact hashes do not match");
    assert!(is_empty_directory(&work.path().join("tampered-evidence")));
}

/// A disposable checkout at its second commit with a candidate for it.
struct Hosted {
    repo: Repo,
    parent: String,
    commit: String,
    environment: Vec<(String, String)>,
}

impl Hosted {
    fn new() -> Self {
        let repo = Repo::new("hamn-hosted-validation-");
        repo.write("README", "first\n");
        let parent = repo.commit_all("first");
        repo.write("README", "second\n");
        let commit = repo.commit_all("second");
        candidate_directory(&repo.scratch("candidate"), RC, &commit, &repo.tree());
        let environment = vec![
            ("RELEASE_REF".into(), commit.clone()),
            ("RELEASE_TAG".into(), RC.into()),
            ("CANDIDATE_DIR".into(), text(&repo.scratch("candidate"))),
            ("OUTPUT_DIR".into(), text(&repo.scratch("evidence/hosted"))),
        ];
        Self { repo, parent, commit, environment }
    }

    fn run(&self, environment: &[(String, String)]) -> Outcome {
        hamn_dev(&["release", "hosted-validation"], self.repo.root(), &pairs(environment))
    }

    /// Validation fails with `message` and writes no evidence.
    fn rejected(&self, environment: &[(String, String)], message: &str) {
        self.run(environment).failed_with(&format!("hosted validation: {message}"));
        let evidence = self.repo.scratch("evidence/hosted/hosted-validation-evidence.json");
        assert!(!evidence.exists(), "{message}: evidence was written");
    }

    fn changed(&self, name: &str, value: Option<&str>) -> Vec<(String, String)> {
        with(&self.environment, name, value)
    }
}

fn local_run_evidence_binds_the_candidate() {
    let hosted = Hosted::new();
    let outcome = hosted.run(&hosted.environment);
    assert!(outcome.succeeded().stdout.contains(&format!("bound hosted validation to exact candidate {RC}")));
    let output = hosted.repo.scratch("evidence/hosted");
    let names: Vec<String> =
        fs::read_dir(&output).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names, ["hosted-validation-evidence.json"]);
    let path = output.join("hosted-validation-evidence.json");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o644);
    let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let candidate = hosted.repo.scratch("candidate");
    let candidate_json: Value = serde_json::from_slice(&fs::read(candidate.join("candidate.json")).unwrap()).unwrap();
    let artifacts: serde_json::Map<String, Value> = candidate_json["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["name"].as_str().unwrap().to_owned(), entry["sha256"].clone()))
        .collect();
    let expected = json!({
        "schemaVersion": 1,
        "kind": "hamn-hosted-validation-evidence",
        "validationMode": "github-hosted-no-vm",
        "physicalE2E": false,
        "tag": RC,
        "commit": hosted.commit,
        "sourceTree": hosted.repo.tree(),
        "workflow": {"run": "local", "attempt": "local"},
        "candidate": {
            "candidateJsonSha256": sha256_file(&candidate.join("candidate.json")).unwrap(),
            "checksumsSha256": sha256_file(&candidate.join("SHA256SUMS")).unwrap(),
            "artifacts": artifacts,
        },
        "checks": hosted_checks(),
    });
    assert_eq!(value, expected);
}

fn workflow_identity_must_be_decimal_or_local() {
    fn identity(hosted: &Hosted, run: Option<&str>, attempt: Option<&str>) -> Vec<(String, String)> {
        with(&with(&hosted.environment, "GITHUB_RUN_ID", run), "GITHUB_RUN_ATTEMPT", attempt)
    }
    let hosted = Hosted::new();
    for (run, attempt) in [
        (Some("0"), Some("1")),
        (Some("01"), Some("1")),
        (Some("1"), Some("0")),
        (Some("1"), None),
        (None, Some("2")),
        (Some("local"), Some("2")),
        (Some("abc"), Some("1")),
        (Some("-1"), Some("1")),
    ] {
        hosted.rejected(&identity(&hosted, run, attempt), "workflow run and attempt must be positive decimals");
    }
    let workflow = |hosted: &Hosted| -> Value {
        let path = hosted.repo.scratch("evidence/hosted/hosted-validation-evidence.json");
        serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["workflow"].clone()
    };
    // Empty values read as unset: a local run.
    hosted.run(&identity(&hosted, Some(""), Some(""))).succeeded();
    assert_eq!(workflow(&hosted), json!({"run": "local", "attempt": "local"}));
    let hosted = Hosted::new();
    hosted.run(&identity(&hosted, Some("417123456"), Some("2"))).succeeded();
    assert_eq!(workflow(&hosted), json!({"run": "417123456", "attempt": "2"}));
}

fn inputs_and_candidate_tag_are_required() {
    let hosted = Hosted::new();
    for name in ["RELEASE_REF", "RELEASE_TAG", "CANDIDATE_DIR", "OUTPUT_DIR"] {
        hosted.rejected(
            &hosted.changed(name, None),
            "RELEASE_REF, RELEASE_TAG, CANDIDATE_DIR, and OUTPUT_DIR are required",
        );
    }
    for tag in ["v0.0.1", "0.0.1-rc.1", "v0.0.1-rc.", "v0.0.1-rc.1a", "v0.0-rc.1", "v0.0.1-rc.1-rc.2", "V0.0.1-rc.1"] {
        hosted.rejected(&hosted.changed("RELEASE_TAG", Some(tag)), "RELEASE_TAG must be a release candidate tag");
    }
    assert!(!hosted.repo.scratch("evidence").exists(), "rejected inputs created OUTPUT_DIR");
}

fn release_ref_must_be_the_checked_out_commit() {
    let hosted = Hosted::new();
    hosted.rejected(
        &hosted.changed("RELEASE_REF", Some(&hosted.parent)),
        "RELEASE_REF does not match the checked-out commit",
    );
    hosted.rejected(&hosted.changed("RELEASE_REF", Some("no-such-ref")), "RELEASE_REF is not a commit");
    assert!(!hosted.repo.scratch("evidence").exists(), "rejected inputs created OUTPUT_DIR");
}

fn directories_must_be_real_and_output_empty() {
    let hosted = Hosted::new();
    let link = hosted.repo.scratch("candidate-link");
    std::os::unix::fs::symlink(hosted.repo.scratch("candidate"), &link).unwrap();
    for path in [text(&link), text(&hosted.repo.scratch("missing"))] {
        hosted.rejected(&hosted.changed("CANDIDATE_DIR", Some(&path)), "CANDIDATE_DIR is unsafe");
    }
    let output = hosted.repo.scratch("evidence/hosted");
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("other"), "kept").unwrap();
    hosted.rejected(&hosted.environment, "OUTPUT_DIR must be empty");
    assert_eq!(fs::read_to_string(output.join("other")).unwrap(), "kept");
    let empty = hosted.repo.scratch("empty");
    fs::create_dir(&empty).unwrap();
    let output_link = hosted.repo.scratch("output-link");
    std::os::unix::fs::symlink(&empty, &output_link).unwrap();
    hosted.rejected(&hosted.changed("OUTPUT_DIR", Some(&text(&output_link))), "OUTPUT_DIR is unsafe");
    assert!(is_empty_directory(&empty));
}

fn candidate_metadata_must_be_regular_files() {
    let hosted = Hosted::new();
    let candidate = hosted.repo.scratch("candidate");
    let moved = hosted.repo.scratch("candidate.json");
    fs::rename(candidate.join("candidate.json"), &moved).unwrap();
    hosted.rejected(&hosted.environment, "candidate metadata is missing");
    std::os::unix::fs::symlink(&moved, candidate.join("candidate.json")).unwrap();
    hosted.rejected(&hosted.environment, "candidate metadata is missing");
    fs::remove_file(candidate.join("candidate.json")).unwrap();
    fs::rename(&moved, candidate.join("candidate.json")).unwrap();
    fs::remove_file(candidate.join("SHA256SUMS")).unwrap();
    hosted.rejected(&hosted.environment, "candidate metadata is missing");
}

fn modified_or_foreign_candidates_are_rejected() {
    let hosted = Hosted::new();
    let candidate = hosted.repo.scratch("candidate");
    let host = candidate.join("hamn-v0.0.1-darwin-arm64.tar.gz");
    let original = fs::read(&host).unwrap();
    fs::write(&host, b"tampered\n").unwrap();
    hosted.rejected(&hosted.environment, "candidate artifact hashes do not match");
    fs::write(&host, &original).unwrap();
    // Intact bytes made for another candidate tag or source.
    let other = hosted.repo.scratch("other");
    let foreign = hosted.changed("CANDIDATE_DIR", Some(&text(&other)));
    candidate_directory(&other, "v0.0.1-rc.1", &hosted.commit, &hosted.repo.tree());
    hosted.rejected(&foreign, "candidate identity does not match hosted validation");
    candidate_directory(&other, RC, &hosted.parent, &hosted.repo.tree());
    hosted.rejected(&foreign, "candidate identity does not match hosted validation");
    // The same inputs with the intact candidate still pass.
    hosted.run(&hosted.environment).succeeded();
}
