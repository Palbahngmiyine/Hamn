//! `release build-candidate` checks every input before it builds or writes
//! anything: the candidate tag, an arm64 builder, the clean checked-out
//! release commit, one owned guest image, the release repository and its
//! canonical manifest URL, and an empty, unlinked output directory. The
//! driver runs in a disposable checkout with a PATH of git and a `uname`
//! fixture only; with every input valid it stops at the missing `make`,
//! and its private workspace is removed. The full build is covered by the
//! hosted-validation and release-artifacts gates.
use crate::runner::{self, case};
use crate::support::release_driver::{Outcome, Repo, hamn_dev, is_empty_directory, pairs, private_bin, text, with};
use std::fs;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-candidate",
        "candidate inputs are checked before anything is built",
        vec![
            case("missing_inputs_and_bad_tags_are_rejected", missing_inputs_and_bad_tags_are_rejected),
            case("only_an_arm64_builder_is_accepted", only_an_arm64_builder_is_accepted),
            case("release_ref_must_be_the_clean_checkout", release_ref_must_be_the_clean_checkout),
            case("guest_image_must_be_one_owned_regular_file", guest_image_must_be_one_owned_regular_file),
            case("repository_and_manifest_url_are_canonical", repository_and_manifest_url_are_canonical),
            case("output_directory_must_be_empty_and_unlinked", output_directory_must_be_empty_and_unlinked),
            case("valid_inputs_reach_the_build_and_clean_up", valid_inputs_reach_the_build_and_clean_up),
        ],
        filters,
    )
}

struct Candidate {
    repo: Repo,
    parent: String,
    environment: Vec<(String, String)>,
}

impl Candidate {
    fn new() -> Self {
        let repo = Repo::new("hamn-release-candidate-");
        repo.write("README", "first\n");
        let parent = repo.commit_all("first");
        repo.write("README", "second\n");
        let commit = repo.commit_all("second");
        let guest = repo.scratch("guest.img");
        fs::write(&guest, "guest image fixture\n").unwrap();
        let bin = private_bin(&repo.scratch(""), &["git"], &["uname"]);
        let environment = vec![
            ("PATH".into(), text(&bin)),
            ("HAMN_DEV_FIXTURE".into(), "release-uname".into()),
            ("RELEASE_REF".into(), commit),
            ("RELEASE_TAG".into(), "v0.0.1-rc.1".into()),
            ("OUTPUT_DIR".into(), text(&repo.scratch("output/candidate"))),
            ("HAMN_GUEST_IMAGE".into(), text(&guest)),
            ("GITHUB_REPOSITORY".into(), "example/hamn".into()),
        ];
        Self { repo, parent, environment }
    }

    fn run(&self, environment: &[(String, String)]) -> Outcome {
        hamn_dev(&["release", "build-candidate"], self.repo.root(), &pairs(environment))
    }

    /// The build fails with `message` and creates no output directory.
    fn rejected(&self, environment: &[(String, String)], message: &str) {
        self.run(environment).failed_with(&format!("release candidate: {message}"));
        assert!(!self.repo.scratch("output").exists(), "{message}: the output directory was created");
    }

    fn changed(&self, name: &str, value: Option<&str>) -> Vec<(String, String)> {
        with(&self.environment, name, value)
    }
}

fn missing_inputs_and_bad_tags_are_rejected() {
    let candidate = Candidate::new();
    for name in ["RELEASE_REF", "RELEASE_TAG", "OUTPUT_DIR"] {
        candidate.rejected(&candidate.changed(name, None), "RELEASE_REF, RELEASE_TAG, and OUTPUT_DIR are required");
    }
    for tag in ["v0.0.1", "0.0.1-rc.1", "v0.0.1-rc.", "v0.0.1-rc.1a", "v0.0-rc.1", "v0.0.1-beta.1"] {
        candidate.rejected(&candidate.changed("RELEASE_TAG", Some(tag)), "RELEASE_TAG must be a vX.Y.Z-rc.N tag");
    }
}

fn only_an_arm64_builder_is_accepted() {
    let candidate = Candidate::new();
    let environment = candidate.changed("HAMN_TEST_MACHINE", Some("x86_64"));
    candidate.rejected(&environment, "release candidate must build on Apple Silicon arm64");
}

fn release_ref_must_be_the_clean_checkout() {
    let candidate = Candidate::new();
    candidate.rejected(&candidate.changed("RELEASE_REF", Some("no-such-ref")), "RELEASE_REF is not a commit");
    let parent = candidate.changed("RELEASE_REF", Some(&candidate.parent));
    candidate.rejected(&parent, "RELEASE_REF does not match the checked-out commit");
    candidate.repo.write("untracked", "x\n");
    candidate.rejected(&candidate.environment, "release source tree is dirty");
    let not_one = candidate.changed("HAMN_RELEASE_ALLOW_DIRTY", Some("true"));
    candidate.rejected(&not_one, "release source tree is dirty");
    // Allowing a dirty tree moves on to the next input.
    let allowed = with(&candidate.changed("HAMN_RELEASE_ALLOW_DIRTY", Some("1")), "HAMN_GUEST_IMAGE", None);
    candidate.rejected(&allowed, "HAMN_GUEST_IMAGE must name one owned regular guest image");
}

fn guest_image_must_be_one_owned_regular_file() {
    let candidate = Candidate::new();
    let guest = candidate.repo.scratch("guest.img");
    let link = candidate.repo.scratch("guest-link.img");
    std::os::unix::fs::symlink(&guest, &link).unwrap();
    let directory = candidate.repo.scratch("guest-directory");
    fs::create_dir(&directory).unwrap();
    let message = "HAMN_GUEST_IMAGE must name one owned regular guest image";
    for path in [None, Some(text(&link)), Some(text(&directory)), Some(text(&candidate.repo.scratch("missing")))] {
        candidate.rejected(&candidate.changed("HAMN_GUEST_IMAGE", path.as_deref()), message);
    }
    // A second name could change the bytes behind the builder's back.
    fs::hard_link(&guest, candidate.repo.scratch("second-name.img")).unwrap();
    candidate.rejected(&candidate.environment, message);
}

fn repository_and_manifest_url_are_canonical() {
    let candidate = Candidate::new();
    let local = |url: Option<&str>, allow_local: Option<&str>| {
        let environment = candidate.changed("GITHUB_REPOSITORY", None);
        with(&with(&environment, "HAMN_RELEASE_MANIFEST_URL", url), "HAMN_RELEASE_ALLOW_LOCAL", allow_local)
    };
    for repository in ["example", "example/hamn/extra", "exa mple/hamn"] {
        let environment = candidate.changed("GITHUB_REPOSITORY", Some(repository));
        candidate.rejected(&environment, "GITHUB_REPOSITORY is invalid");
    }
    // HAMN_RELEASE_REPOSITORY is the repository outside GitHub Actions.
    let fallback = with(&candidate.changed("GITHUB_REPOSITORY", None), "HAMN_RELEASE_REPOSITORY", Some("example"));
    candidate.rejected(&fallback, "GITHUB_REPOSITORY is invalid");
    let other = candidate.changed("HAMN_RELEASE_MANIFEST_URL", Some("https://downloads.example.invalid/manifest.json"));
    candidate.rejected(&other, "HAMN_RELEASE_MANIFEST_URL must match the canonical GitHub Release manifest URL");
    candidate.rejected(&local(None, Some("1")), "HAMN_RELEASE_MANIFEST_URL is required outside GitHub Actions");
    candidate.rejected(&local(Some("http://example.invalid/m.json"), Some("1")), "release manifest URL must use HTTPS");
    candidate.rejected(&local(Some("file:///tmp/m.json"), None), "release manifest URL must use HTTPS");
    candidate.rejected(&local(Some("/tmp/m.json"), Some("0")), "release manifest URL must use HTTPS");
    // Without a repository, artifact URLs are local files, for tests only.
    candidate.rejected(&local(Some("https://example.invalid/m.json"), None), "HAMN_RELEASE_REPOSITORY is required");
}

fn output_directory_must_be_empty_and_unlinked() {
    let candidate = Candidate::new();
    let output = candidate.repo.scratch("output/candidate");
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("previous"), "kept").unwrap();
    candidate.run(&candidate.environment).failed_with("release candidate: OUTPUT_DIR must be empty");
    assert_eq!(fs::read_to_string(output.join("previous")).unwrap(), "kept");
    let empty = candidate.repo.scratch("empty");
    fs::create_dir(&empty).unwrap();
    let link = candidate.repo.scratch("output-link");
    std::os::unix::fs::symlink(&empty, &link).unwrap();
    let linked = candidate.changed("OUTPUT_DIR", Some(&text(&link)));
    candidate.run(&linked).failed_with("release candidate: OUTPUT_DIR is unsafe");
    assert!(is_empty_directory(&empty));
}

fn valid_inputs_reach_the_build_and_clean_up() {
    let candidate = Candidate::new();
    let outcome = candidate.run(&candidate.environment);
    outcome.failed_with("release candidate: make: ");
    // The private workspace inside OUTPUT_DIR is removed on failure.
    assert!(is_empty_directory(&candidate.repo.scratch("output/candidate")), "{outcome:#?}");
    // Local artifact URLs are accepted only when allowed.
    let local = with(
        &with(&candidate.changed("GITHUB_REPOSITORY", None), "HAMN_RELEASE_MANIFEST_URL", Some("file:///tmp/m.json")),
        "HAMN_RELEASE_ALLOW_LOCAL",
        Some("1"),
    );
    fs::remove_dir(candidate.repo.scratch("output/candidate")).unwrap();
    candidate.run(&local).failed_with("release candidate: make: ");
}
