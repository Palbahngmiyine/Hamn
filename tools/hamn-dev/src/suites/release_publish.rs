//! `release publish` promotes exact hosted candidate bytes without
//! rebuilding them or using a long-lived key.
//!
//! The first cases share a real candidate of this checkout, built by the
//! first of them with `release build-candidate` (which rebuilds build/hamn
//! as v0.0.1; it is restored when the suite ends) and bound by `release
//! hosted-validation` to workflow run 417123456 attempt 2, as the release
//! workflow does. Its guest image size evidence is synthetic and accepted
//! only through the explicit local test budget, and its published manifest
//! is consumed by the release-publisher-consumer suite. The archive case
//! promotes synthetic candidates of this checkout; run from the repository
//! root. The remaining cases promote synthetic candidates of disposable
//! checkouts, where valid inputs stop at the size gate: those checkouts
//! have no guest image tool to build.
use crate::release::files::{canonical_json, sha256_file};
use crate::runner::{self, case};
use crate::support::exec::output_within;
use crate::support::release_driver::{
    Outcome, Repo, RestoreHost, bind_candidate, candidate_artifacts, candidate_directory, git, hamn_dev,
    is_empty_directory, outcome, pairs, release_command, text, with,
};
use crate::support::tmp::TempDir;
use serde_json::{Value, json};
use std::cell::OnceCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::panic;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let built = Rc::new(Built::default());
    let on_built = |run: fn(&Published)| {
        let built = Rc::clone(&built);
        move || run(built.get())
    };
    runner::run(
        "release-publish",
        "keyless promotion verifies hosted evidence without rebuilding",
        vec![
            case("built_candidate_is_published_unchanged", on_built(built_candidate_is_published_unchanged)),
            case(
                "size_evidence_must_be_reviewed_for_the_commit",
                on_built(size_evidence_must_be_reviewed_for_the_commit),
            ),
            case("foreign_or_overstated_evidence_is_rejected", on_built(foreign_or_overstated_evidence_is_rejected)),
            case("candidate_client_must_accept_the_manifest", on_built(candidate_client_must_accept_the_manifest)),
            case("host_archive_must_hold_one_executable", host_archive_must_hold_one_executable),
            case("arguments_and_tags_are_checked", arguments_and_tags_are_checked),
            case("provenance_is_a_workflow_run_or_solo_local", provenance_is_a_workflow_run_or_solo_local),
            case("release_base_url_is_canonical_and_https", release_base_url_is_canonical_and_https),
            case("directories_and_candidate_files_must_be_safe", directories_and_candidate_files_must_be_safe),
            case("release_ref_must_be_the_candidate_commit", release_ref_must_be_the_candidate_commit),
            case(
                "test_size_budget_is_forbidden_in_release_workflows",
                test_size_budget_is_forbidden_in_release_workflows,
            ),
        ],
        filters,
    )
}

const STABLE: &str = "v0.0.1";
const RC: &str = "v0.0.1-rc.417123456";
const RUN: &str = "417123456";
const ATTEMPT: &str = "2";
const REPOSITORY: &str = "example/hamn";
const HOST: &str = "hamn-v0.0.1-darwin-arm64.tar.gz";
const GUEST: &str = "hamn-v0.0.1-ubuntu-24.04-arm64.img";
const MANIFEST: &str = "hamn-update-manifest-v3.json";
/// Where promotion fails once every earlier input was accepted, when the
/// size evidence (or, in a disposable checkout, the image tool) is missing.
const SIZE_GATE: &str = "guest image size evidence or reviewed release budget is missing or invalid";
/// The minimum savings a release image must show, 64 MiB.
const SAVED_BYTES: u64 = 64 * 1024 * 1024;
/// Release driver inputs that the caller's environment must not supply.
const DRIVER_INPUTS: [&str; 9] = [
    "HAMN_RELEASE_PROVENANCE",
    "HAMN_EXPECTED_WORKFLOW_RUN",
    "HAMN_EXPECTED_WORKFLOW_ATTEMPT",
    "HAMN_RELEASE_REPOSITORY",
    "HAMN_RELEASE_BASE_URL",
    "HAMN_TEST_RELEASE_SIZE_BUDGET",
    "HAMN_RELEASE_ALLOW_LOCAL",
    "HAMN_RELEASE_ALLOW_DIRTY",
    "HAMN_RELEASE_MANIFEST_URL",
];

/// This executable as a release driver of this checkout: our environment
/// without a hosted workflow's identity or the drivers' inputs, plus
/// `extra`.
fn driver(extra: &[(&str, &str)]) -> Command {
    let mut command = release_command(&[]);
    for name in DRIVER_INPUTS {
        command.env_remove(name);
    }
    command.envs(extra.iter().copied());
    command
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))).unwrap()
}

/// The sorted names in `directory`.
fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(directory).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    names
}

/// A guest image size report for `image` built at `revision`, as the image
/// builder writes one, 64 MiB below a synthetic baseline. Synthetic fixture
/// evidence exercises the gate; it is never production image measurement
/// and is accepted only through the explicit local test budget.
fn size_report(image: &Path, revision: &str) -> Value {
    let bytes = fs::metadata(image).unwrap().len();
    json!({
        "schemaVersion": 1, "reviewOnly": false, "compressedBytes": bytes,
        "imageSha256": sha256_file(image).unwrap(), "baselineCompressedBytes": bytes + SAVED_BYTES,
        "baselineSha256": "1".repeat(64), "savedBytes": SAVED_BYTES, "requiredSavingsBytes": SAVED_BYTES,
        "virtualBytes": 8u64 << 30, "baseImageSha256": "2".repeat(64), "sourceRevision": revision,
        "packagesBefore": ["fixture"], "packagesAfter": ["fixture"], "cleanup": ["fixture only"],
        "runtimeValidation": "not executed",
    })
}

/// Writes `report` as the size report in `evidence`; returns its path.
fn write_report(evidence: &Path, report: &Value) -> PathBuf {
    let path = evidence.join("guest-image-size-report.json");
    fs::write(&path, serde_json::to_vec_pretty(report).unwrap()).unwrap();
    path
}

/// Writes a size budget at `path` that allows `maximum` compressed bytes
/// of `image` and names `report` as its reviewed footprint.
fn write_budget(path: &Path, image: &Path, report: &Path, maximum: u64) {
    let budget = json!({
        "schemaVersion": 1, "maximumCompressedBytes": maximum,
        "referenceImageSha256": sha256_file(image).unwrap(), "footprintReportSha256": sha256_file(report).unwrap(),
    });
    fs::write(path, serde_json::to_vec_pretty(&budget).unwrap()).unwrap();
}

/// Promotion inputs of this checkout in a private directory:
/// `input/hamn-candidate`, `input/hamn-evidence` and a local test budget.
struct Inputs {
    work: TempDir,
}

impl Inputs {
    fn new() -> Self {
        Self { work: TempDir::new("hamn-release-publish-") }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.work.path().join(name)
    }

    fn candidate(&self) -> PathBuf {
        self.path("input/hamn-candidate")
    }

    fn evidence(&self) -> PathBuf {
        self.path("input/hamn-evidence")
    }

    fn budget(&self) -> PathBuf {
        self.path("size-budget.json")
    }

    /// A new, empty OUTPUT_DIR.
    fn output(&self, name: &str) -> PathBuf {
        let output = self.path(name);
        fs::create_dir(&output).unwrap();
        output
    }

    /// Records the candidate's hosted evidence of run 417123456 attempt 2,
    /// as the candidate job does.
    fn validate_hosted(&self, release_ref: &str) {
        let (candidate, evidence) = (text(&self.candidate()), text(&self.evidence()));
        let mut command = driver(&[
            ("RELEASE_REF", release_ref),
            ("RELEASE_TAG", RC),
            ("CANDIDATE_DIR", &candidate),
            ("OUTPUT_DIR", &evidence),
            ("GITHUB_RUN_ID", RUN),
            ("GITHUB_RUN_ATTEMPT", ATTEMPT),
        ]);
        command.args(["release", "hosted-validation"]);
        outcome(output_within(&mut command, Duration::from_secs(300))).succeeded();
    }

    /// Size evidence of the candidate's guest image at `revision` and a
    /// test budget one byte above it.
    fn record_size(&self, revision: &str) {
        let image = self.candidate().join(GUEST);
        let report = write_report(&self.evidence(), &size_report(&image, revision));
        write_budget(&self.budget(), &image, &report, fs::metadata(&image).unwrap().len() + 1);
    }

    /// The release workflow's promotion environment plus the local test
    /// budget.
    fn environment(&self) -> Vec<(String, String)> {
        [
            ("HAMN_RELEASE_REPOSITORY", REPOSITORY),
            ("HAMN_EXPECTED_WORKFLOW_RUN", RUN),
            ("HAMN_EXPECTED_WORKFLOW_ATTEMPT", ATTEMPT),
            ("HAMN_RELEASE_ALLOW_LOCAL", "1"),
            ("HAMN_TEST_RELEASE_SIZE_BUDGET", &text(&self.budget())),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .to_vec()
    }

    /// `release publish v0.0.1 RC COMMIT INPUT OUTPUT` in this checkout.
    fn publish(&self, commit: &str, output: &Path, environment: &[(String, String)]) -> Outcome {
        let mut command = driver(&pairs(environment));
        command.args(["release", "publish", STABLE, RC, commit, &text(&self.path("input")), &text(output)]);
        outcome(output_within(&mut command, Duration::from_secs(1200)))
    }
}

/// A real candidate of this checkout's HEAD, bound as the release workflow
/// binds it.
struct Published {
    inputs: Inputs,
    release_ref: String,
    tree: String,
    // Declared last, so dropped last: build/hamn is rebuilt after the
    // candidate is removed.
    _restore: RestoreHost,
}

impl Published {
    fn build() -> Self {
        let restore = RestoreHost::capture();
        let inputs = Inputs::new();
        let release_ref = git(Path::new("."), &["rev-parse", "HEAD"]);
        let tree = git(Path::new("."), &["rev-parse", "HEAD^{tree}"]);
        let guest = inputs.path("guest.img");
        fs::write(&guest, "immutable guest image fixture\n").unwrap();
        let (candidate, guest) = (text(&inputs.candidate()), text(&guest));
        let mut build = driver(&[
            ("GITHUB_REPOSITORY", REPOSITORY),
            ("RELEASE_REF", &release_ref),
            ("RELEASE_TAG", RC),
            ("OUTPUT_DIR", &candidate),
            ("HAMN_GUEST_IMAGE", &guest),
            ("HAMN_RELEASE_ALLOW_DIRTY", "1"),
        ]);
        build.args(["release", "build-candidate"]);
        outcome(output_within(&mut build, Duration::from_secs(3600))).succeeded();
        inputs.validate_hosted(&release_ref);
        inputs.record_size(&release_ref);
        Self { inputs, release_ref, tree, _restore: restore }
    }

    fn publish(&self, output: &Path, environment: &[(String, String)]) -> Outcome {
        self.inputs.publish(&self.release_ref, output, environment)
    }

    /// Promotion into the new OUTPUT_DIR `name` fails with `message` and
    /// writes nothing.
    fn rejected(&self, name: &str, environment: &[(String, String)], message: &str) -> Outcome {
        let output = self.inputs.output(name);
        let outcome = self.publish(&output, environment);
        outcome.failed_with(&format!("hamn publish: {message}"));
        assert!(is_empty_directory(&output), "{name}: promotion wrote {:?}", entries(&output));
        outcome
    }
}

/// The real candidate, built by the first case that needs it. A failed
/// build fails that case and every later case that needs it; it is not
/// retried.
#[derive(Default)]
struct Built(OnceCell<Option<Published>>);

impl Built {
    fn get(&self) -> &Published {
        let built = self.0.get_or_init(|| panic::catch_unwind(Published::build).ok());
        built.as_ref().expect("the real candidate could not be built; see the first failure")
    }
}

/// Puts back a file's bytes, or its absence, when dropped, so a case that
/// changes a shared input leaves later cases the original even when it
/// fails.
struct Restore {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}

impl Restore {
    fn new(path: &Path) -> Self {
        Self { path: path.to_owned(), bytes: fs::read(path).ok() }
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        let restored = match &self.bytes {
            Some(bytes) => fs::write(&self.path, bytes),
            None => fs::remove_file(&self.path),
        };
        if let Err(error) = restored {
            eprintln!("release-publish: cannot restore {}: {error}", self.path.display());
        }
    }
}

fn built_candidate_is_published_unchanged(published: &Published) {
    let inputs = &published.inputs;
    let candidate = inputs.candidate();
    let (host, guest) = (candidate.join(HOST), candidate.join(GUEST));
    let host_sha256 = sha256_file(&host).unwrap();
    let output = inputs.output("publish");
    let published_run = published.publish(&output, &inputs.environment());
    let verified = format!("verified hosted candidate {RC}; publish exact bytes without rebuilding");
    assert!(published_run.succeeded().stdout.lines().any(|line| line == verified), "{published_run:#?}");
    assert_eq!(sha256_file(&host).unwrap(), host_sha256, "promotion changed the candidate bytes");
    // Keyless: no signature, and only the schema v3 manifest.
    for absent in ["hamn-update-manifest.json.sig", "validation-evidence.json.sig", "hamn-update-manifest.json"] {
        assert!(!output.join(absent).exists(), "{absent} was published");
    }
    let names = entries(&output);
    assert_eq!(
        names,
        ["SHA256SUMS", "candidate.json", MANIFEST, "hosted-validation-evidence.json", "promoted-from-rc"]
    );
    for name in &names {
        let mode = fs::metadata(output.join(name)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o644, "{name}");
    }
    for (name, source) in [
        ("candidate.json", candidate.join("candidate.json")),
        ("SHA256SUMS", candidate.join("SHA256SUMS")),
        ("hosted-validation-evidence.json", inputs.evidence().join("hosted-validation-evidence.json")),
    ] {
        assert_eq!(fs::read(output.join(name)).unwrap(), fs::read(source).unwrap(), "{name} is not the exact input");
    }
    assert_eq!(fs::read_to_string(output.join("promoted-from-rc")).unwrap(), format!("{RC}\n"));

    let base = format!("https://github.com/{REPOSITORY}/releases/download/{STABLE}");
    let size = |path: &Path| fs::metadata(path).unwrap().len();
    let expected = json!({
        "schemaVersion": 3, "channel": "stable", "version": STABLE, "commit": published.release_ref,
        "validationMode": "github-hosted-no-vm",
        "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
        "artifacts": {
            "host": {"url": format!("{base}/{HOST}"), "sha256": host_sha256, "size": size(&host)},
            "guestImage": {"url": format!("{base}/{GUEST}"), "sha256": sha256_file(&guest).unwrap(),
                "size": size(&guest), "format": "qcow2", "compression": "zlib", "virtualSize": 8_589_934_592u64},
        },
    });
    let manifest = read_json(&output.join(MANIFEST));
    assert_eq!(manifest, expected, "keyless update manifest does not bind the exact candidate");
    let evidence = read_json(&output.join("hosted-validation-evidence.json"));
    assert!(
        evidence["physicalE2E"] == json!(false) && evidence["sourceTree"] == json!(published.tree),
        "hosted evidence overstates validation: {evidence}"
    );

    // Publisher URLs and bytes stay unchanged; native curl reaches them
    // through a bounded local TLS CONNECT fixture with child-only trust.
    let root = std::env::current_dir().unwrap();
    let mut consumer = driver(&[
        ("HAMN_SOURCE_ROOT", &text(&root)),
        ("HAMN_PUBLISHED_DIR", &text(&output)),
        ("HAMN_CANDIDATE_DIR", &text(&candidate)),
        ("HAMN_CONSUMER_WORK", &text(&inputs.path("consumers"))),
    ]);
    consumer.args(["test", "release-publisher-consumer"]);
    outcome(output_within(&mut consumer, Duration::from_secs(900))).succeeded();
}

fn size_evidence_must_be_reviewed_for_the_commit(published: &Published) {
    let inputs = &published.inputs;
    let image = inputs.candidate().join(GUEST);
    let bytes = fs::metadata(&image).unwrap().len();
    let _report = Restore::new(&inputs.evidence().join("guest-image-size-report.json"));
    let original = size_report(&image, &published.release_ref);
    let budget = |budget: &Path| with(&inputs.environment(), "HAMN_TEST_RELEASE_SIZE_BUDGET", Some(&text(budget)));
    let rejected = |name: &str, budget_path: &Path, reason: &str| {
        published.rejected(name, &budget(budget_path), SIZE_GATE).failed_with(reason);
    };
    // Missing reviewed evidence must fail at publication, even when every
    // candidate and hosted-validation identity is valid.
    let missing = "regular image, size report and reviewed release-size-budget.json are required";
    rejected("missing-budget", &inputs.path("absent-budget.json"), missing);
    let mut review_only = original.clone();
    review_only["reviewOnly"] = json!(true);
    write_report(&inputs.evidence(), &review_only);
    rejected("review-only", &inputs.budget(), "review-only or invalid image report cannot be published");
    // Evidence of another source revision, with a budget naming it.
    let mut foreign = original.clone();
    foreign["sourceRevision"] = json!(published.tree);
    let report = write_report(&inputs.evidence(), &foreign);
    let foreign_budget = inputs.path("foreign-budget.json");
    write_budget(&foreign_budget, &image, &report, bytes + 1);
    rejected("foreign-revision", &foreign_budget, "size report belongs to a different source revision");
    // The budget bounds the exact image bytes: at the maximum is accepted.
    let report = write_report(&inputs.evidence(), &original);
    let tight = inputs.path("tight-budget.json");
    write_budget(&tight, &image, &report, bytes - 1);
    rejected("over-budget", &tight, "image bytes, savings or reviewed budget do not match size evidence");
    let exact = inputs.path("exact-budget.json");
    write_budget(&exact, &image, &report, bytes);
    published.publish(&inputs.output("at-budget"), &budget(&exact)).succeeded();
}

fn foreign_or_overstated_evidence_is_rejected(published: &Published) {
    let inputs = &published.inputs;
    let environment = inputs.environment();
    let provenance = "hosted validation workflow provenance mismatch";
    published.rejected("wrong-run", &with(&environment, "HAMN_EXPECTED_WORKFLOW_RUN", Some("417123457")), provenance);
    published.rejected("wrong-attempt", &with(&environment, "HAMN_EXPECTED_WORKFLOW_ATTEMPT", Some("3")), provenance);
    {
        let path = inputs.evidence().join("hosted-validation-evidence.json");
        let _evidence = Restore::new(&path);
        let mut overstated = read_json(&path);
        overstated["physicalE2E"] = json!(true);
        fs::write(&path, canonical_json(&overstated)).unwrap();
        published.rejected("overstated", &environment, "hosted validation identity mismatch");
    }
    let unbound = inputs.candidate().join("unbound.txt");
    let _unbound = Restore::new(&unbound);
    fs::write(&unbound, "unbound data\n").unwrap();
    published.rejected("extra", &environment, "candidate artifact directory contains unexpected entries");
}

fn candidate_client_must_accept_the_manifest(published: &Published) {
    let inputs = &published.inputs;
    let configured = |base: &str| {
        let local = with(&inputs.environment(), "HAMN_RELEASE_REPOSITORY", None);
        with(&local, "HAMN_RELEASE_BASE_URL", Some(base))
    };
    // The base passes the driver's HTTPS check, but the candidate's parser
    // refuses user information in artifact URLs; nothing else is published.
    let output = inputs.output("rejected-by-client");
    let rejected = published.publish(&output, &configured("https://user@downloads.example.test/hamn"));
    rejected.failed_with("hamn publish: the candidate client rejects the generated v3 manifest");
    assert_eq!(entries(&output), [MANIFEST]);
    let manifest = read_json(&output.join(MANIFEST));
    assert_eq!(manifest["artifacts"]["host"]["url"], format!("https://user@downloads.example.test/hamn/{HOST}"));
    // A configured base is used as given.
    let base = "https://downloads.example.test/hamn/v0.0.1";
    let output = inputs.output("configured-base");
    published.publish(&output, &configured(base)).succeeded();
    let manifest = read_json(&output.join(MANIFEST));
    assert_eq!(manifest["artifacts"]["host"]["url"], format!("{base}/{HOST}"));
    assert_eq!(manifest["artifacts"]["guestImage"]["url"], format!("{base}/{GUEST}"));
}

/// A gzip tar archive of `members`, each a regular file with mode 0755.
fn archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (path, data) in members {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn host_archive_must_hold_one_executable() {
    let release_ref = git(Path::new("."), &["rev-parse", "HEAD"]);
    let tree = git(Path::new("."), &["rev-parse", "HEAD^{tree}"]);
    let root = "hamn-v0.0.1-darwin-arm64";
    let (bin, other, nested) = (format!("{root}/bin/hamn"), format!("{root}/bin/other"), format!("{root}/x/bin/hamn"));
    for (host, message) in [
        (archive(&[(&other, b"x")]), "candidate host archive has no executable"),
        (archive(&[(&nested, b"x")]), "candidate host archive has no executable"),
        (archive(&[("a/bin/hamn", b"x"), ("b/bin/hamn", b"x")]), "candidate host archive has duplicate executables"),
        (b"not a gzip tar archive\n".to_vec(), "cannot list the candidate host archive"),
        (archive(&[(&bin, b"\0not an executable")]), "the candidate client rejects the generated v3 manifest"),
    ] {
        let inputs = Inputs::new();
        let candidate = inputs.candidate();
        fs::create_dir_all(&candidate).unwrap();
        for name in candidate_artifacts(RC) {
            fs::write(candidate.join(&name), format!("fixture {name}\n")).unwrap();
        }
        fs::write(candidate.join(HOST), &host).unwrap();
        bind_candidate(&candidate, RC, &release_ref, &tree);
        inputs.validate_hosted(&release_ref);
        inputs.record_size(&release_ref);
        let output = inputs.output("publish");
        inputs.publish(&release_ref, &output, &inputs.environment()).failed_with(&format!("hamn publish: {message}"));
        // The manifest is written, and nothing else, before the client runs.
        assert_eq!(entries(&output), [MANIFEST], "{message}");
    }
}

/// A disposable checkout at its second commit with a synthetic candidate
/// of it in `input/hamn-candidate`, hosted evidence of run 417123456
/// attempt 2 in `input/hamn-evidence` and an empty `output`. Valid inputs
/// stop at the size gate: the checkout has no guest image tool to build.
struct Disposable {
    repo: Repo,
    parent: String,
    commit: String,
    environment: Vec<(String, String)>,
}

impl Disposable {
    fn new() -> Self {
        let repo = Repo::new("hamn-release-publish-");
        repo.write("README", "first\n");
        let parent = repo.commit_all("first");
        repo.write("README", "second\n");
        let commit = repo.commit_all("second");
        candidate_directory(&repo.scratch("input/hamn-candidate"), RC, &commit, &repo.tree());
        fs::create_dir(repo.scratch("output")).unwrap();
        let environment = [
            ("HAMN_RELEASE_REPOSITORY", REPOSITORY),
            ("HAMN_EXPECTED_WORKFLOW_RUN", RUN),
            ("HAMN_EXPECTED_WORKFLOW_ATTEMPT", ATTEMPT),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .to_vec();
        let disposable = Self { repo, parent, commit, environment };
        disposable.record_evidence(Some((RUN, ATTEMPT)));
        disposable
    }

    fn path(&self, name: &str) -> PathBuf {
        self.repo.scratch(name)
    }

    /// Replaces the hosted evidence with evidence of `run` and attempt, or
    /// of a local run.
    fn record_evidence(&self, run: Option<(&str, &str)>) {
        let evidence = self.path("input/hamn-evidence");
        if evidence.exists() {
            fs::remove_dir_all(&evidence).unwrap();
        }
        let (candidate, evidence) = (text(&self.path("input/hamn-candidate")), text(&evidence));
        let mut environment =
            vec![("RELEASE_REF", &self.commit[..]), ("RELEASE_TAG", RC), ("CANDIDATE_DIR", &candidate)];
        environment.push(("OUTPUT_DIR", &evidence));
        if let Some((run, attempt)) = run {
            environment.extend([("GITHUB_RUN_ID", run), ("GITHUB_RUN_ATTEMPT", attempt)]);
        }
        hamn_dev(&["release", "hosted-validation"], self.repo.root(), &environment).succeeded();
    }

    /// `v0.0.1 RC COMMIT INPUT OUTPUT`, with argument `index` replaced by
    /// `value` when given.
    fn arguments(&self, replaced: Option<(usize, &str)>) -> Vec<String> {
        let mut args = [STABLE, RC, &self.commit, &text(&self.path("input")), &text(&self.path("output"))]
            .map(str::to_owned)
            .to_vec();
        if let Some((index, value)) = replaced {
            args[index] = value.to_owned();
        }
        args
    }

    fn run(&self, args: &[String], environment: &[(String, String)]) -> Outcome {
        let mut words = vec!["release", "publish"];
        words.extend(args.iter().map(String::as_str));
        hamn_dev(&words, self.repo.root(), &pairs(environment))
    }

    /// Promotion fails with `message` and writes nothing to `output`.
    fn rejected(&self, args: &[String], environment: &[(String, String)], message: &str) {
        self.run(args, environment).failed_with(&format!("hamn publish: {message}"));
        assert!(is_empty_directory(&self.path("output")), "{message}: promotion wrote output");
    }

    /// Every input before the size gate is accepted.
    fn reaches_size_gate(&self, args: &[String], environment: &[(String, String)]) {
        self.rejected(args, environment, SIZE_GATE);
    }

    fn changed(&self, name: &str, value: Option<&str>) -> Vec<(String, String)> {
        with(&self.environment, name, value)
    }
}

fn arguments_and_tags_are_checked() {
    let fixture = Disposable::new();
    let (args, environment) = (fixture.arguments(None), &fixture.environment);
    let usage = "usage: hamn-dev release publish vX.Y.Z vX.Y.Z-rc.N COMMIT INPUT_DIR OUTPUT_DIR";
    for count in 0..args.len() {
        fixture.rejected(&args[..count], environment, usage);
    }
    fixture.rejected(&[args.clone(), vec!["extra".into()]].concat(), environment, usage);
    for index in 0..args.len() {
        fixture.rejected(&fixture.arguments(Some((index, ""))), environment, usage);
    }
    for stable in ["0.0.1", "v0.0", "v0.0.1.1", "v0.0.1-rc.1", "V0.0.1"] {
        fixture.rejected(&fixture.arguments(Some((0, stable))), environment, "stable tag is invalid");
    }
    // The stable tag is matched literally: its dots match nothing else.
    for candidate in ["v0.0.2-rc.417123456", "v0.0.1-rc.", "v0.0.1", "v0x0x1-rc.417123456"] {
        let args = fixture.arguments(Some((1, candidate)));
        fixture.rejected(&args, environment, "RC tag does not correspond to the stable tag");
    }
    fixture.reaches_size_gate(&args, environment);
}

fn provenance_is_a_workflow_run_or_solo_local() {
    let fixture = Disposable::new();
    let args = fixture.arguments(None);
    for value in [None, Some("0"), Some("01"), Some("local")] {
        let run = fixture.changed("HAMN_EXPECTED_WORKFLOW_RUN", value);
        fixture.rejected(&args, &run, "HAMN_EXPECTED_WORKFLOW_RUN must be a positive decimal run ID");
        let attempt = fixture.changed("HAMN_EXPECTED_WORKFLOW_ATTEMPT", value);
        fixture.rejected(&args, &attempt, "HAMN_EXPECTED_WORKFLOW_ATTEMPT must be a positive decimal attempt");
    }
    let unknown = fixture.changed("HAMN_RELEASE_PROVENANCE", Some("local"));
    fixture.rejected(&args, &unknown, "HAMN_RELEASE_PROVENANCE must be workflow or solo-local");
    fixture.reaches_size_gate(&args, &fixture.changed("HAMN_RELEASE_PROVENANCE", Some("workflow")));
    // The expected run is the one that recorded the evidence.
    let other = fixture.changed("HAMN_EXPECTED_WORKFLOW_ATTEMPT", Some("3"));
    fixture.rejected(&args, &other, "hosted validation workflow provenance mismatch");
    let solo = fixture.changed("HAMN_RELEASE_PROVENANCE", Some("solo-local"));
    fixture.rejected(&args, &solo, "solo-local provenance must not accept workflow run inputs");
    let solo = with(&with(&solo, "HAMN_EXPECTED_WORKFLOW_RUN", None), "HAMN_EXPECTED_WORKFLOW_ATTEMPT", None);
    let inside = with(&solo, "GITHUB_ACTIONS", Some("true"));
    fixture.rejected(&args, &inside, "solo-local provenance is unavailable inside GitHub Actions");
    // Solo-local promotion expects evidence of a local run, and only that.
    fixture.rejected(&args, &solo, "hosted validation workflow provenance mismatch");
    fixture.record_evidence(None);
    fixture.reaches_size_gate(&args, &solo);
    fixture.rejected(&args, &fixture.environment, "hosted validation workflow provenance mismatch");
}

fn release_base_url_is_canonical_and_https() {
    let fixture = Disposable::new();
    let args = fixture.arguments(None);
    let invalid = fixture.changed("HAMN_RELEASE_REPOSITORY", Some("example/hamn/extra"));
    fixture.rejected(&args, &invalid, "HAMN_RELEASE_REPOSITORY is invalid");
    let canonical = "https://github.com/example/hamn/releases/download/v0.0.1";
    let overridden = fixture.changed("HAMN_RELEASE_BASE_URL", Some(canonical));
    fixture.rejected(&args, &overridden, "HAMN_RELEASE_BASE_URL must not override the canonical GitHub Release base");
    let local = fixture.changed("HAMN_RELEASE_REPOSITORY", None);
    fixture.rejected(&args, &local, "HAMN_RELEASE_BASE_URL is required outside GitHub Actions");
    let insecure = with(&local, "HAMN_RELEASE_BASE_URL", Some("http://downloads.example.invalid/v0.0.1"));
    fixture.rejected(&args, &insecure, "release base URL must use HTTPS");
    let configured = with(&local, "HAMN_RELEASE_BASE_URL", Some("https://downloads.example.invalid/v0.0.1"));
    fixture.reaches_size_gate(&args, &configured);
}

fn directories_and_candidate_files_must_be_safe() {
    let fixture = Disposable::new();
    let (args, environment) = (fixture.arguments(None), &fixture.environment);
    let candidate = fixture.path("input/hamn-candidate");
    let input_link = fixture.path("input-link");
    std::os::unix::fs::symlink(fixture.path("input"), &input_link).unwrap();
    for input in [fixture.path("missing"), input_link, candidate.join("install.sh")] {
        fixture.rejected(&fixture.arguments(Some((3, &text(&input)))), environment, "INPUT_DIR is unsafe");
    }
    // OUTPUT_DIR must already exist; promotion never creates it.
    let missing = fixture.path("missing-output");
    fixture.rejected(&fixture.arguments(Some((4, &text(&missing)))), environment, "OUTPUT_DIR is unsafe");
    assert!(!missing.exists(), "promotion created OUTPUT_DIR");
    let empty = fixture.path("empty");
    fs::create_dir(&empty).unwrap();
    let output_link = fixture.path("output-link");
    std::os::unix::fs::symlink(&empty, &output_link).unwrap();
    fixture.rejected(&fixture.arguments(Some((4, &text(&output_link)))), environment, "OUTPUT_DIR is unsafe");
    assert!(is_empty_directory(&empty));
    let previous = fixture.path("output/previous");
    fs::write(&previous, "kept").unwrap();
    fixture.run(&args, environment).failed_with("hamn publish: OUTPUT_DIR must be empty");
    assert_eq!(fs::read_to_string(&previous).unwrap(), "kept");
    fs::remove_file(&previous).unwrap();

    for directory in ["hamn-candidate", "hamn-evidence"] {
        let path = fixture.path(&format!("input/{directory}"));
        let moved = fixture.path(directory);
        fs::rename(&path, &moved).unwrap();
        fixture.rejected(&args, environment, "candidate or hosted evidence directory is missing");
        std::os::unix::fs::symlink(&moved, &path).unwrap();
        fixture.rejected(&args, environment, "candidate or hosted evidence directory is missing");
        fs::remove_file(&path).unwrap();
        fs::rename(&moved, &path).unwrap();
    }

    // Every file promotion reads is a regular file with one name, not a
    // link. (Another owner cannot be arranged without privileges.)
    let mut files: Vec<PathBuf> = ["candidate.json".to_owned(), "SHA256SUMS".to_owned()]
        .into_iter()
        .chain(candidate_artifacts(RC))
        .map(|name| candidate.join(name))
        .collect();
    files.push(fixture.path("input/hamn-evidence/hosted-validation-evidence.json"));
    let moved = fixture.path("moved");
    for file in &files {
        let unsafe_input = format!("unsafe release input: {}", file.display());
        fs::rename(file, &moved).unwrap();
        fixture.rejected(&args, environment, &unsafe_input);
        std::os::unix::fs::symlink(&moved, file).unwrap();
        fixture.rejected(&args, environment, &unsafe_input);
        fs::remove_file(file).unwrap();
        fs::create_dir(file).unwrap();
        fixture.rejected(&args, environment, &unsafe_input);
        fs::remove_dir(file).unwrap();
        fs::rename(&moved, file).unwrap();
        let second = fixture.path("second-name");
        fs::hard_link(file, &second).unwrap();
        fixture.rejected(&args, environment, &unsafe_input);
        fs::remove_file(&second).unwrap();
    }

    let host = candidate.join(HOST);
    let original = fs::read(&host).unwrap();
    fs::write(&host, b"tampered\n").unwrap();
    fixture.rejected(&args, environment, "candidate artifact hashes do not match");
    fs::write(&host, original).unwrap();
    fixture.reaches_size_gate(&args, environment);
}

fn release_ref_must_be_the_candidate_commit() {
    let fixture = Disposable::new();
    let environment = &fixture.environment;
    let reference = |value: &str| fixture.arguments(Some((2, value)));
    fixture.rejected(&reference("no-such-ref"), environment, "RELEASE_REF is not a commit");
    fixture.rejected(&reference(&fixture.repo.tree()), environment, "RELEASE_REF is not a commit");
    // The candidate and its evidence name the release commit, not another,
    // even one of the same source tree.
    fixture.rejected(&reference(&fixture.parent), environment, "candidate provenance mismatch");
    let tree = fixture.repo.tree();
    let sibling = git(fixture.repo.root(), &["commit-tree", &tree, "-p", &fixture.commit, "-m", "same tree"]);
    fixture.rejected(&reference(&sibling), environment, "candidate provenance mismatch");
    fixture.reaches_size_gate(&reference("main"), environment);
}

fn test_size_budget_is_forbidden_in_release_workflows() {
    let fixture = Disposable::new();
    let args = fixture.arguments(None);
    let forbidden = "test size budget is forbidden in release workflows";
    let budget = text(&fixture.path("size-budget.json"));
    let test_budget = fixture.changed("HAMN_TEST_RELEASE_SIZE_BUDGET", Some(&budget));
    fixture.rejected(&args, &test_budget, forbidden);
    fixture.rejected(&args, &with(&test_budget, "HAMN_RELEASE_ALLOW_LOCAL", Some("true")), forbidden);
    let local = with(&test_budget, "HAMN_RELEASE_ALLOW_LOCAL", Some("1"));
    for actions in ["true", "false"] {
        fixture.rejected(&args, &with(&local, "GITHUB_ACTIONS", Some(actions)), forbidden);
    }
    fixture.reaches_size_gate(&args, &local);
    // An empty test budget is no test budget.
    let unset = with(&fixture.changed("HAMN_TEST_RELEASE_SIZE_BUDGET", Some("")), "GITHUB_ACTIONS", Some("true"));
    fixture.reaches_size_gate(&args, &unset);
}
