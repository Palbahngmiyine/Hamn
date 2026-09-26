//! Release Please manifest transitions gate automated releases: the
//! checkout's version copies agree, and `release resolve-release` (the
//! push trigger) turns only an increased, consistent manifest version at
//! the checked-out commit into one release run. Run from the repository
//! root; transitions use disposable checkouts.
use crate::release::version::check_version_state;
use crate::runner::{self, case};
use crate::support::release_driver::{Outcome, Repo, hamn_dev, pairs, text, with};
use std::fs;
use std::path::Path;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-version",
        "Release Please manifest transitions gate automated releases",
        vec![
            case("checkout_version_state_is_consistent", checkout_version_state_is_consistent),
            case("bootstrap_and_released_states_are_accepted", bootstrap_and_released_states_are_accepted),
            case("each_version_state_inconsistency_is_named", each_version_state_inconsistency_is_named),
            case("bootstrap_transition_needs_no_release", bootstrap_transition_needs_no_release),
            case("manifest_increase_resolves_one_release_run", manifest_increase_resolves_one_release_run),
            case(
                "mismatched_copies_and_non_increasing_versions_are_rejected",
                mismatched_copies_and_non_increasing_versions_are_rejected,
            ),
            case("workflow_identity_and_outputs_are_validated", workflow_identity_and_outputs_are_validated),
        ],
        filters,
    )
}

fn state(root: &Path) -> Result<(), String> {
    check_version_state(&[text(root)])
}

fn checkout_version_state_is_consistent() {
    state(Path::new(".")).unwrap();
}

/// Release Please configuration and version copies, as a checkout has them.
struct StateFixture {
    manifest: &'static str,
    source: &'static str,
    make: &'static str,
    flake: &'static str,
    bump_minor: bool,
    initial: &'static str,
}

impl Default for StateFixture {
    fn default() -> Self {
        Self { manifest: "0.0.1", source: "0.0.1", make: "0.0.1", flake: "0.0.1", bump_minor: true, initial: "0.0.1" }
    }
}

impl StateFixture {
    fn write(&self, root: &Path) {
        fs::create_dir_all(root).unwrap();
        let config = format!(
            "{{\"initial-version\":\"{}\",\"bump-minor-pre-major\":{},\"bump-patch-for-minor-pre-major\":true}}\n",
            self.initial, self.bump_minor
        );
        fs::write(root.join("release-please-config.json"), config).unwrap();
        fs::write(root.join(".release-please-manifest.json"), format!("{{\".\":\"{}\"}}\n", self.manifest)).unwrap();
        fs::write(root.join("version.txt"), format!("{}\n", self.source)).unwrap();
        fs::write(root.join("Makefile"), makefile(self.make)).unwrap();
        fs::write(root.join("flake.nix"), flake(self.flake)).unwrap();
    }
}

fn makefile(version: &str) -> String {
    format!("# x-release-please-start-version\nVERSION    ?= {version}\n# x-release-please-end\n")
}

fn flake(version: &str) -> String {
    format!("      hamnVersion = \"{version}\"; # x-release-please-version\n")
}

fn bootstrap_and_released_states_are_accepted() {
    let repo = Repo::new("hamn-release-version-");
    let root = repo.scratch("state");
    StateFixture { manifest: "0.0.0", ..StateFixture::default() }.write(&root);
    state(&root).unwrap();
    StateFixture::default().write(&root);
    state(&root).unwrap();
}

fn each_version_state_inconsistency_is_named() {
    let repo = Repo::new("hamn-release-version-");
    let root = repo.scratch("state");
    let rejected = |fixture: StateFixture, change: &dyn Fn(&Path), message: &str| {
        fixture.write(&root);
        change(&root);
        let error = state(&root).expect_err(message);
        assert!(error.contains(message), "{message:?}: {error}");
    };
    let none = |_: &Path| {};
    let write =
        |name: &'static str, contents: &'static str| move |root: &Path| fs::write(root.join(name), contents).unwrap();
    rejected(
        StateFixture::default(),
        &write("release-please-config.json", "[]\n"),
        "Release Please configuration must be an object",
    );
    rejected(
        StateFixture { initial: "0.0.2", ..StateFixture::default() },
        &none,
        "initial release version policy is not 0.0.1",
    );
    rejected(
        StateFixture::default(),
        &write(".release-please-manifest.json", "{\".\":\"0.0.1\",\"guest\":\"0.0.1\"}\n"),
        "release manifest must contain only the root package",
    );
    rejected(
        StateFixture { manifest: "01.0.0", ..StateFixture::default() },
        &none,
        "manifest version is not canonical SemVer",
    );
    rejected(
        StateFixture { source: "01.0.0", make: "01.0.0", flake: "01.0.0", ..StateFixture::default() },
        &none,
        "version.txt is not one canonical SemVer line",
    );
    rejected(
        StateFixture::default(),
        &write("version.txt", "0.0.1\n0.0.2\n"),
        "version.txt is not one canonical SemVer line",
    );
    rejected(
        StateFixture { manifest: "0.0.0", source: "0.0.2", make: "0.0.2", flake: "0.0.2", ..StateFixture::default() },
        &none,
        "bootstrap source version does not match initial-version",
    );
    rejected(
        StateFixture { source: "0.0.2", make: "0.0.2", flake: "0.0.2", ..StateFixture::default() },
        &none,
        "released source version does not match the manifest",
    );
    rejected(
        StateFixture { make: "0.0.2", ..StateFixture::default() },
        &none,
        "Makefile version does not match version.txt",
    );
    rejected(
        StateFixture { flake: "0.0.2", ..StateFixture::default() },
        &none,
        "flake.nix version does not match version.txt",
    );
    rejected(
        StateFixture { bump_minor: false, ..StateFixture::default() },
        &none,
        "pre-major release policy is incomplete",
    );
    rejected(
        StateFixture::default(),
        &|root: &Path| {
            fs::remove_file(root.join("flake.nix")).unwrap();
            std::os::unix::fs::symlink(root.join("Makefile"), root.join("flake.nix")).unwrap();
        },
        "release version source is missing or unsafe: flake.nix",
    );
}

/// A checkout whose history is: sources without Release Please, then the
/// Release Please bootstrap manifest (0.0.0).
struct History {
    repo: Repo,
    base: String,
    bootstrap: String,
}

impl History {
    fn new() -> Self {
        let repo = Repo::new("hamn-release-version-");
        Self::sources(&repo, "0.0.1");
        let base = repo.commit_all("base without Release Please");
        repo.write(".release-please-manifest.json", "{\".\":\"0.0.0\"}\n");
        let bootstrap = repo.commit_all("bootstrap Release Please");
        Self { repo, base, bootstrap }
    }

    fn sources(repo: &Repo, version: &str) {
        repo.write("version.txt", &format!("{version}\n"));
        repo.write("Makefile", &makefile(version));
        repo.write("flake.nix", &flake(version));
    }

    /// Commits the manifest and source versions; returns the commit.
    fn release(&self, manifest: &str, source: &str) -> String {
        Self::sources(&self.repo, source);
        self.repo.write(".release-please-manifest.json", &format!("{{\".\":\"{manifest}\"}}\n"));
        self.repo.commit_all(&format!("release {manifest}"))
    }

    /// A new, empty step output file.
    fn output(&self) -> String {
        let path = self.repo.scratch("github-output");
        fs::write(&path, "").unwrap();
        text(&path)
    }

    /// The push workflow's environment for the checked-out commit.
    fn environment(&self, output: &str) -> Vec<(String, String)> {
        vec![
            ("GITHUB_OUTPUT".into(), output.into()),
            ("GITHUB_RUN_ID".into(), "417123456".into()),
            ("GITHUB_SHA".into(), self.repo.head()),
        ]
    }

    fn resolve(&self, previous: &str, environment: &[(String, String)]) -> Outcome {
        hamn_dev(&["release", "resolve-release", previous], self.repo.root(), &pairs(environment))
    }

    fn outputs(&self) -> String {
        fs::read_to_string(self.repo.scratch("github-output")).unwrap()
    }
}

fn bootstrap_transition_needs_no_release() {
    let history = History::new();
    let output = history.output();
    let outcome = history.resolve(&history.base, &history.environment(&output));
    assert!(outcome.succeeded().stdout.contains("Release Please bootstrap detected; no release is due"));
    assert_eq!(history.outputs(), "should_release=false\n");
    // GitHub's all-zero ref names no previous manifest, even when the
    // checkout has one; the bootstrap needs no workflow run identity.
    history.release("0.0.1", "0.0.1");
    let output = history.output();
    let environment = with(&history.environment(&output), "GITHUB_RUN_ID", None);
    history.resolve(&"0".repeat(40), &environment).succeeded();
    assert_eq!(history.outputs(), "should_release=false\n");
}

fn manifest_increase_resolves_one_release_run() {
    let history = History::new();
    let release = history.release("0.0.1", "0.0.1");
    let output = history.output();
    let outcome = history.resolve(&history.bootstrap, &history.environment(&output));
    assert!(outcome.succeeded().stdout.contains("automated release version resolved from Release Please manifest"));
    assert_eq!(
        history.outputs(),
        format!(
            "should_release=true\nversion=0.0.1\nstable_tag=v0.0.1\ncandidate_tag=v0.0.1-rc.417123456\ncommit={release}\n"
        )
    );
}

fn mismatched_copies_and_non_increasing_versions_are_rejected() {
    let history = History::new();
    let rejected = |message: &str| {
        let output = history.output();
        history.resolve(&history.bootstrap, &history.environment(&output)).failed_with(message);
        assert_eq!(history.outputs(), "", "a rejected transition appended outputs");
    };
    history.release("0.0.1", "0.0.2");
    rejected("version.txt does not match the release manifest");
    History::sources(&history.repo, "0.0.1");
    history.repo.write("Makefile", &makefile("0.0.2"));
    history.repo.commit_all("mismatched Makefile");
    rejected("Makefile does not match the release manifest");
    History::sources(&history.repo, "0.0.1");
    history.repo.write("flake.nix", &flake("0.0.2"));
    history.repo.commit_all("mismatched flake");
    rejected("flake.nix does not match the release manifest");
    history.release("0.0.0", "0.0.1");
    rejected("release version did not increase");
    history.release("01.0.0", "01.0.0");
    rejected("release version is not canonical SemVer");
}

fn workflow_identity_and_outputs_are_validated() {
    let history = History::new();
    let release = history.release("0.0.1", "0.0.1");
    let output = history.output();
    let environment = history.environment(&output);
    let rejected = |previous: &str, environment: Vec<(String, String)>, message: &str| {
        history.resolve(previous, &environment).failed_with(&format!("automated release: {message}"));
        assert_eq!(history.outputs(), "", "{message}: outputs were appended");
    };
    for previous in ["", "HEAD~1", &history.bootstrap[..39], &history.bootstrap.to_uppercase()] {
        rejected(previous, environment.clone(), "previous release ref must be a full commit SHA");
    }
    for run in [None, Some(""), Some("0"), Some("01"), Some("-1"), Some("1.5")] {
        let changed = with(&environment, "GITHUB_RUN_ID", run);
        rejected(&history.bootstrap, changed, "GITHUB_RUN_ID must be a positive decimal integer");
    }
    for sha in [None, Some("HEAD"), Some(&release[..39])] {
        let changed = with(&environment, "GITHUB_SHA", sha);
        rejected(&history.bootstrap, changed, "GITHUB_SHA must be a full commit SHA");
    }
    let other = with(&environment, "GITHUB_SHA", Some(&history.bootstrap));
    rejected(&history.bootstrap, other, "GITHUB_SHA does not match the checked-out commit");
    let link = history.repo.scratch("output-link");
    std::os::unix::fs::symlink(&output, &link).unwrap();
    let directory = history.repo.scratch("output-directory");
    fs::create_dir(&directory).unwrap();
    for path in [None, Some(text(&history.repo.scratch("missing"))), Some(text(&link)), Some(text(&directory))] {
        let changed = with(&environment, "GITHUB_OUTPUT", path.as_deref());
        rejected(&history.bootstrap, changed, "GITHUB_OUTPUT must name an existing regular file");
    }
}
