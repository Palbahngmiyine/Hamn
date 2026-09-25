//! `release recover-release` re-requests an unpublished manifest version
//! only from a workflow_dispatch on protected main at the checked-out
//! commit, and never once its stable tag exists. Disposable checkouts keep
//! the developer checkout's tags and version out of the result.
use crate::runner::{self, case};
use crate::support::release_driver::{Outcome, Repo, hamn_dev, pairs, text, with};
use std::fs;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-request",
        "unpublished release recovery is pinned to protected main",
        vec![
            case(
                "dispatch_on_main_recovers_the_unpublished_version",
                dispatch_on_main_recovers_the_unpublished_version,
            ),
            case("recovery_requires_dispatch_main_and_the_checkout", recovery_requires_dispatch_main_and_the_checkout),
            case("published_stable_tag_is_not_recovered", published_stable_tag_is_not_recovered),
            case("inconsistent_version_copies_are_not_recovered", inconsistent_version_copies_are_not_recovered),
        ],
        filters,
    )
}

struct Fixture {
    repo: Repo,
    commit: String,
}

impl Fixture {
    /// A checkout whose manifest and version copies all say 0.1.0.
    fn new() -> Self {
        let repo = Repo::new("hamn-release-request-");
        repo.write(".release-please-manifest.json", "{\".\":\"0.1.0\"}\n");
        repo.write("version.txt", "0.1.0\n");
        repo.write("Makefile", "VERSION ?= 0.1.0\n");
        repo.write("flake.nix", "hamnVersion = \"0.1.0\"; # x-release-please-version\n");
        let commit = repo.commit_all("fixture");
        Self { repo, commit }
    }

    fn environment(&self) -> Vec<(String, String)> {
        let output = self.repo.scratch("github-output");
        fs::write(&output, "").unwrap();
        vec![
            ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
            ("GITHUB_REF".into(), "refs/heads/main".into()),
            ("GITHUB_SHA".into(), self.commit.clone()),
            ("GITHUB_RUN_ID".into(), "417123456".into()),
            ("GITHUB_OUTPUT".into(), text(&output)),
        ]
    }

    fn recover(&self, environment: &[(String, String)]) -> Outcome {
        hamn_dev(&["release", "recover-release"], self.repo.root(), &pairs(environment))
    }

    fn outputs(&self) -> String {
        fs::read_to_string(self.repo.scratch("github-output")).unwrap()
    }

    /// Recovery fails with `message` and appends nothing.
    fn rejected(&self, environment: &[(String, String)], message: &str) {
        self.recover(environment).failed_with(&format!("release request: {message}"));
        assert_eq!(self.outputs(), "", "{message}: outputs were appended");
    }
}

fn dispatch_on_main_recovers_the_unpublished_version() {
    let fixture = Fixture::new();
    let outcome = fixture.recover(&fixture.environment());
    assert!(outcome.succeeded().stdout.contains("recovered unpublished release v0.1.0 from protected main"));
    assert_eq!(
        fixture.outputs(),
        format!(
            "should_release=true\nversion=0.1.0\nstable_tag=v0.1.0\ncandidate_tag=v0.1.0-rc.417123456\ncommit={}\n",
            fixture.commit
        )
    );
}

fn recovery_requires_dispatch_main_and_the_checkout() {
    let fixture = Fixture::new();
    let environment = fixture.environment();
    for event in [None, Some("push"), Some("workflow_dispatch ")] {
        let changed = with(&environment, "GITHUB_EVENT_NAME", event);
        fixture.rejected(&changed, "only workflow_dispatch may recover an unpublished release");
    }
    for reference in [None, Some("refs/heads/feature"), Some("refs/tags/v0.1.0"), Some("main")] {
        fixture.rejected(&with(&environment, "GITHUB_REF", reference), "release recovery must run from main");
    }
    for sha in [None, Some("HEAD"), Some(&fixture.commit[..39])] {
        fixture.rejected(&with(&environment, "GITHUB_SHA", sha), "GITHUB_SHA must be a full commit SHA");
    }
    fixture.repo.commit_all("later");
    fixture.rejected(&environment, "GITHUB_SHA does not match the checked-out commit");
    let environment = with(&environment, "GITHUB_SHA", Some(&fixture.repo.head()));
    for run in [None, Some("0"), Some("01"), Some("x")] {
        fixture.rejected(&with(&environment, "GITHUB_RUN_ID", run), "GITHUB_RUN_ID must be a positive decimal");
    }
    let link = fixture.repo.scratch("output-link");
    std::os::unix::fs::symlink(fixture.repo.scratch("github-output"), &link).unwrap();
    for output in [None, Some(text(&link)), Some(text(&fixture.repo.scratch("missing")))] {
        let changed = with(&environment, "GITHUB_OUTPUT", output.as_deref());
        fixture.rejected(&changed, "GITHUB_OUTPUT must name an existing regular file");
    }
}

fn published_stable_tag_is_not_recovered() {
    let fixture = Fixture::new();
    fixture.repo.write("unrelated", "x\n");
    let tagged = fixture.repo.commit_all("tagged");
    crate::support::release_driver::git(fixture.repo.root(), &["tag", "v0.1.0", &tagged]);
    let environment = with(&fixture.environment(), "GITHUB_SHA", Some(&tagged));
    fixture.rejected(&environment, "stable tag already exists: v0.1.0");
}

fn inconsistent_version_copies_are_not_recovered() {
    let fixture = Fixture::new();
    fixture.repo.write("version.txt", "0.1.1\n");
    let commit = fixture.repo.commit_all("mismatch");
    let environment = with(&fixture.environment(), "GITHUB_SHA", Some(&commit));
    fixture.rejected(&environment, "cannot resolve the current release version: version.txt does not match");
}
