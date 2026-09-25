//! `release export-public-source` creates one root commit of the exact
//! checked-out tree, with no remote, and never absorbs uncommitted or
//! untracked content such as a user-owned `desktop/`. Disposable checkouts
//! exercise the boundaries; one case exports this checkout (run from the
//! repository root).
use crate::runner::{self, case};
use crate::support::release_driver::{Outcome, Repo, git, hamn_dev, text};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "public-export",
        "public source export is single-root and remote-free",
        vec![
            case("export_is_one_root_commit_of_the_committed_tree", export_is_one_root_commit_of_the_committed_tree),
            case("existing_destination_is_never_reused", existing_destination_is_never_reused),
            case("destination_must_be_absolute_with_a_real_parent", destination_must_be_absolute_with_a_real_parent),
            case("tracked_desktop_assets_are_refused", tracked_desktop_assets_are_refused),
            case("usage_errors_exit_2", usage_errors_exit_2),
            case("this_checkout_exports_its_head_tree", this_checkout_exports_its_head_tree),
        ],
        filters,
    )
}

fn export(repo_root: &Path, output: &Path) -> Outcome {
    hamn_dev(&["release", "export-public-source", &text(output)], repo_root, &[])
}

/// A checkout with an executable, a link and nested files committed, then
/// uncommitted and untracked changes, including a `desktop/` tree.
fn source() -> (Repo, String) {
    let repo = Repo::new("hamn-public-export-");
    repo.write("README", "committed\n");
    repo.write("scripts/run.sh", "#!/bin/sh\nexit 0\n");
    fs::set_permissions(repo.root().join("scripts/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("scripts/run.sh", repo.root().join("run")).unwrap();
    repo.write("docs/deep/guide.md", "guide\n");
    repo.commit_all("source");
    repo.write("README", "uncommitted\n");
    repo.write("untracked.txt", "untracked\n");
    repo.write("desktop/App.swift", "user-owned\n");
    let tree = git(repo.root(), &["rev-parse", "HEAD^{tree}"]);
    (repo, tree)
}

fn export_is_one_root_commit_of_the_committed_tree() {
    let (repo, tree) = source();
    let output = repo.scratch("public-hamn");
    let outcome = export(repo.root(), &output);
    let commit = repo.head();
    assert_eq!(outcome.succeeded().stdout, format!("exported {commit} as one root commit at {}\n", text(&output)));
    assert_eq!(git(&output, &["rev-list", "--all", "--count"]), "1");
    assert_eq!(git(&output, &["rev-parse", "HEAD^{tree}"]), tree, "the export tree differs from the source commit");
    assert_eq!(git(&output, &["rev-list", "--max-parents=0", "HEAD"]), git(&output, &["rev-parse", "HEAD"]));
    assert_eq!(git(&output, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(
        git(&output, &["log", "-1", "--format=%an <%ae>|%cn <%ce>|%s"]),
        "Hamn Release Export <release-export@invalid>|Hamn Release Export <release-export@invalid>|Initial Hamn 0.0.1 source"
    );
    assert_eq!(git(&output, &["remote"]), "", "the export configured a remote");
    assert_eq!(git(&output, &["status", "--porcelain"]), "");
    git(&output, &["fsck", "--no-reflogs"]);
    assert!(!output.join("desktop").exists(), "the export included untracked desktop assets");
    assert!(!output.join("untracked.txt").exists(), "the export included an untracked file");
    assert_eq!(fs::read_to_string(output.join("README")).unwrap(), "committed\n");
    assert_eq!(fs::metadata(&output).unwrap().permissions().mode() & 0o7777, 0o755);
}

fn existing_destination_is_never_reused() {
    let (repo, _) = source();
    let output = repo.scratch("public-hamn");
    export(repo.root(), &output).succeeded();
    let head = git(&output, &["rev-parse", "HEAD"]);
    export(repo.root(), &output).failed_with("public export: output directory already exists");
    assert_eq!(git(&output, &["rev-parse", "HEAD"]), head);
    // A link is an existing destination too, even when it dangles.
    let dangling = repo.scratch("dangling");
    std::os::unix::fs::symlink(repo.scratch("nowhere"), &dangling).unwrap();
    export(repo.root(), &dangling).failed_with("output directory already exists");
    assert!(!repo.scratch("nowhere").exists());
    let empty = repo.scratch("empty");
    fs::create_dir(&empty).unwrap();
    export(repo.root(), &empty).failed_with("output directory already exists");
}

fn destination_must_be_absolute_with_a_real_parent() {
    let (repo, _) = source();
    let relative = hamn_dev(&["release", "export-public-source", "public-hamn"], repo.root(), &[]);
    relative.failed_with("public export: output directory must be absolute");
    assert!(!repo.root().join("public-hamn").exists());
    let parent = repo.scratch("parent");
    fs::create_dir(&parent).unwrap();
    let link = repo.scratch("parent-link");
    std::os::unix::fs::symlink(&parent, &link).unwrap();
    export(repo.root(), &link.join("public-hamn")).failed_with("output parent directory is unsafe");
    assert!(!parent.join("public-hamn").exists());
    export(repo.root(), &repo.scratch("missing/public-hamn")).failed_with("output parent directory is unsafe");
    assert!(!repo.scratch("missing").exists());
}

fn tracked_desktop_assets_are_refused() {
    let repo = Repo::new("hamn-public-export-");
    repo.write("README", "x\n");
    repo.write("desktop/App.swift", "tracked\n");
    repo.commit_all("with desktop");
    let output = repo.scratch("public-hamn");
    export(repo.root(), &output).failed_with("source commit still contains tracked desktop assets");
    assert!(!output.exists());
}

fn usage_errors_exit_2() {
    let repo = Repo::new("hamn-public-export-");
    for args in [&["release", "export-public-source"][..], &["release", "export-public-source", "/a", "/b"]] {
        let outcome = hamn_dev(args, repo.root(), &[]);
        assert_eq!(outcome.code, Some(2), "{outcome:#?}");
        assert!(outcome.stderr.contains("usage: hamn-dev release export-public-source OUTPUT_DIRECTORY"));
    }
}

fn this_checkout_exports_its_head_tree() {
    let work = Repo::new("hamn-public-export-");
    let output = work.scratch("public-hamn");
    let root = std::env::current_dir().unwrap();
    export(&root, &output).succeeded();
    assert_eq!(git(&output, &["rev-parse", "HEAD^{tree}"]), git(&root, &["rev-parse", "HEAD^{tree}"]));
    assert_eq!(git(&output, &["rev-list", "--all", "--count"]), "1");
    assert!(!output.join("desktop").exists());
}
