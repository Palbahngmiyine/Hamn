//! Shared fixtures of the release driver suites: disposable Git checkouts,
//! candidate directories bound to them, private `bin/` directories and a
//! hermetic, deadline-bounded `hamn-dev release ...` runner.
//!
//! Every Git command here and in the drivers under test runs with
//! `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1`, so no user or
//! system configuration (signing, hooks, templates) changes the outcome.
use super::exec::{output_within, which};
use super::tmp::TempDir;
use crate::release::files::{canonical_json, sha256_file};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

/// Git configuration and identity isolation for every child.
const GIT_ISOLATION: [(&str, &str); 6] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_AUTHOR_NAME", "Hamn Test"),
    ("GIT_AUTHOR_EMAIL", "hamn-test@invalid"),
    ("GIT_COMMITTER_NAME", "Hamn Test"),
    ("GIT_COMMITTER_EMAIL", "hamn-test@invalid"),
];

/// Runs `git ARGS` in `directory`, which must succeed; returns its standard
/// output without trailing newlines.
pub fn git(directory: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.arg("-C").arg(directory).args(args).envs(GIT_ISOLATION);
    let output = output_within(&mut command, Duration::from_secs(60));
    assert!(output.status.success(), "git {args:?} in {}: {output:?}", directory.display());
    String::from_utf8(output.stdout).expect("git output is UTF-8").trim_end_matches('\n').to_owned()
}

/// A disposable checkout in a private directory that also holds the
/// test's other files; both are removed when dropped.
pub struct Repo {
    scratch: TempDir,
    root: PathBuf,
}

impl Repo {
    pub fn new(prefix: &str) -> Self {
        let scratch = TempDir::new(prefix);
        // Canonical, as `git rev-parse --show-toplevel` reports it.
        let root = scratch.path().canonicalize().unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "--initial-branch", "main"]);
        Self { scratch, root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A path in the private directory, outside the checkout.
    pub fn scratch(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    pub fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Commits every change and returns the new commit.
    pub fn commit_all(&self, message: &str) -> String {
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-q", "--allow-empty", "-m", message]);
        self.head()
    }

    pub fn head(&self) -> String {
        git(&self.root, &["rev-parse", "HEAD"])
    }

    pub fn tree(&self) -> String {
        git(&self.root, &["rev-parse", "HEAD^{tree}"])
    }
}

/// A private `bin/` in `directory` holding links to the real `tools` (from
/// our PATH) and to this executable as each of `fixtures`, whose behavior
/// `HAMN_DEV_FIXTURE` selects. A driver given only this PATH can run
/// nothing else.
pub fn private_bin(directory: &Path, tools: &[&str], fixtures: &[&str]) -> PathBuf {
    let bin = directory.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for tool in tools {
        let real = which(tool).unwrap_or_else(|| panic!("{tool} is required on PATH"));
        std::os::unix::fs::symlink(real, bin.join(tool)).unwrap();
    }
    for fixture in fixtures {
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), bin.join(fixture)).unwrap();
    }
    bin
}

/// A finished driver run.
#[derive(Debug)]
pub struct Outcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    pub fn succeeded(&self) -> &Self {
        assert_eq!(self.code, Some(0), "{self:#?}");
        self
    }

    /// Exit status 1 and `message` in the diagnostics.
    pub fn failed_with(&self, message: &str) -> &Self {
        assert!(self.code == Some(1) && self.stderr.contains(message), "expected failure {message:?}: {self:#?}");
        self
    }
}

/// `hamn-dev ARGS` in `directory` with exactly `environment`, Git
/// isolation and, unless `environment` sets them, our PATH and a HOME
/// that does not exist. Killed and failed after `timeout`.
pub fn hamn_dev_within(args: &[&str], directory: &Path, environment: &[(&str, &str)], timeout: Duration) -> Outcome {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(args).current_dir(directory).env_clear();
    command.env("PATH", std::env::var_os("PATH").unwrap_or_default()).env("HOME", "/nonexistent/hamn-test-home");
    command.envs(GIT_ISOLATION).envs(environment.iter().copied());
    outcome(output_within(&mut command, timeout))
}

pub fn hamn_dev(args: &[&str], directory: &Path, environment: &[(&str, &str)]) -> Outcome {
    hamn_dev_within(args, directory, environment, Duration::from_secs(120))
}

pub fn outcome(output: std::process::Output) -> Outcome {
    Outcome {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Environment pairs as the runner takes them.
pub fn pairs(owned: &[(String, String)]) -> Vec<(&str, &str)> {
    owned.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect()
}

/// `owned` with `name` replaced by `value`, or removed when `value` is
/// `None`.
pub fn with(owned: &[(String, String)], name: &str, value: Option<&str>) -> Vec<(String, String)> {
    let mut changed: Vec<(String, String)> = owned.iter().filter(|(key, _)| key != name).cloned().collect();
    if let Some(value) = value {
        changed.push((name.to_owned(), value.to_owned()));
    }
    changed
}

pub fn text(path: &Path) -> String {
    path.to_str().expect("test paths are UTF-8").to_owned()
}

/// Whether `path` is a directory with no entries.
pub fn is_empty_directory(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// Writes a candidate directory for `tag` (`vX.Y.Z-rc.N`) at the given
/// source whose metadata satisfies both the physical and the hosted
/// candidate contracts; returns `candidate.json`'s value.
pub fn candidate_directory(directory: &Path, tag: &str, commit: &str, tree: &str) -> Value {
    let version = tag.split("-rc.").next().unwrap();
    fs::create_dir_all(directory).unwrap();
    let names = [
        format!("hamn-{version}-darwin-arm64.tar.gz"),
        format!("hamn-{version}-ubuntu-24.04-arm64.img"),
        format!("hamn-{version}.spdx.json"),
        "install.sh".to_owned(),
    ];
    for name in &names {
        fs::write(directory.join(name), format!("fixture {name}\n")).unwrap();
    }
    let artifacts: Vec<Value> =
        names.iter().map(|name| json!({"name": name, "sha256": sha256_file(&directory.join(name)).unwrap()})).collect();
    let candidate = json!({"schemaVersion": 1, "kind": "hamn-release-candidate", "tag": tag, "version": version,
        "commit": commit, "sourceTree": tree, "artifacts": artifacts});
    fs::write(directory.join("candidate.json"), canonical_json(&candidate)).unwrap();
    let checksums: String = names
        .iter()
        .map(String::as_str)
        .chain(["candidate.json"])
        .map(|name| format!("{}  {name}\n", sha256_file(&directory.join(name)).unwrap()))
        .collect();
    fs::write(directory.join("SHA256SUMS"), checksums).unwrap();
    candidate
}

/// Fixture `uname`: `uname -m` prints `$HAMN_TEST_MACHINE` (default
/// arm64); anything else fails with status 2.
pub fn uname(_program: &str, args: &[String]) -> ExitCode {
    if args != ["-m"] {
        return ExitCode::from(2);
    }
    println!("{}", std::env::var("HAMN_TEST_MACHINE").unwrap_or_else(|_| "arm64".into()));
    ExitCode::SUCCESS
}

/// PATH value of `directories`.
pub fn search_path(directories: &[&Path]) -> OsString {
    std::env::join_paths(directories).unwrap()
}
