//! `export-public-source OUTPUT_DIRECTORY`: a local, single-root
//! public-source repository from the exact checked-out commit.
//!
//! Only the commit's tree is exported (`git archive`): uncommitted and
//! untracked content, such as a user-owned `desktop/`, never is, and a
//! commit that still tracks `desktop/` is refused. The export never adds or
//! changes a remote, renames a repository or deletes private history; those
//! externally visible steps need an operator's decision after inspecting
//! the export. OUTPUT_DIRECTORY must be absolute and must not exist; a
//! failure after it was created leaves it for inspection.
use super::checkout::{Checkout, git_in, git_text};
use super::files::{Workspace, set_mode};
use super::process::{self, Spec};
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::time::Duration;

pub const USAGE: &str = "usage: hamn-dev release export-public-source OUTPUT_DIRECTORY

Create OUTPUT_DIRECTORY as a new Git repository with exactly one root commit
whose tree is identical to the checked-out Hamn source commit. The destination
must not exist. No remote is added or changed.";

/// Bounds each git or tar step over the whole source tree.
const TREE_TIMEOUT: Duration = Duration::from_secs(600);

/// The export commit's author and committer.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Hamn Release Export"),
    ("GIT_AUTHOR_EMAIL", "release-export@invalid"),
    ("GIT_COMMITTER_NAME", "Hamn Release Export"),
    ("GIT_COMMITTER_EMAIL", "release-export@invalid"),
];

/// `export-public-source OUTPUT_DIRECTORY`. A usage error exits with status
/// 2, as the shell script this replaces did.
pub fn main(args: &[String]) -> Result<(), String> {
    let [output] = args else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    export(Path::new(output)).map_err(|error| format!("public export: {error}"))
}

fn export(output: &Path) -> Result<(), String> {
    if !output.is_absolute() {
        return Err("output directory must be absolute".into());
    }
    match fs::symlink_metadata(output) {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Ok(_) => return Err("output directory already exists".into()),
        Err(error) => return Err(format!("{}: {error}", output.display())),
    }
    let parent = output.parent().ok_or("output parent directory is unsafe")?;
    if !fs::symlink_metadata(parent).is_ok_and(|info| info.file_type().is_dir()) {
        return Err("output parent directory is unsafe".into());
    }
    let checkout = Checkout::current()?;
    let commit = checkout.commit("HEAD").map_err(|_| "cannot resolve checked-out commit")?;
    let tree = checkout.tree(&commit).map_err(|_| "cannot resolve checked-out source tree")?;
    // Desktop is untracked and user-owned in the CLI-only repository; make
    // the tracked boundary loud.
    if !checkout.git_text(&["ls-tree", "-r", "--name-only", &commit, "--", "desktop"])?.is_empty() {
        return Err("source commit still contains tracked desktop assets".into());
    }

    fs::DirBuilder::new().mode(0o755).create(output).map_err(|error| format!("{}: {error}", output.display()))?;
    set_mode(output, 0o755)?;
    extract(&checkout, &commit, output).map_err(|error| format!("cannot extract tracked source tree: {error}"))?;
    let git = |args: &[&str]| git_text(output, args, TREE_TIMEOUT);
    git(&["init", "-q", "--initial-branch", "main"])?;
    git(&["add", "-A"])?;
    let committed = git_in(output, &["commit", "-qm", "Initial Hamn 0.0.1 source"], &IDENTITY, TREE_TIMEOUT)?;
    if !committed.status.success() {
        return Err(format!("cannot commit the public export: {}", committed.stderr_lossy().trim()));
    }
    if git(&["rev-list", "--all", "--count"])? != "1" {
        return Err("public export does not have exactly one root commit".into());
    }
    if git(&["rev-parse", "HEAD^{tree}"])? != tree {
        return Err("public export tree does not match the source commit".into());
    }
    git(&["fsck", "--no-reflogs"]).map_err(|error| format!("public export repository is invalid: {error}"))?;
    println!("exported {commit} as one root commit at {}", output.display());
    Ok(())
}

/// Writes `commit`'s tree into `output` through a tar file in a private
/// workspace outside it.
fn extract(checkout: &Checkout, commit: &str, output: &Path) -> Result<(), String> {
    let work = Workspace::create(&std::env::temp_dir(), "hamn-public-export.")?;
    let archive = work.path().join("source.tar");
    let archive_text = archive.to_str().ok_or("temporary directory path is not UTF-8")?;
    let written = git_in(&checkout.root, &["archive", "--format=tar", "-o", archive_text, commit], &[], TREE_TIMEOUT)?;
    if !written.status.success() {
        return Err(format!("git archive failed: {}", written.stderr_lossy().trim()));
    }
    let environment = super::checkout::environment(&[])?;
    let args: [&OsStr; 5] = ["-x".as_ref(), "-f".as_ref(), archive.as_os_str(), "-C".as_ref(), output.as_os_str()];
    process::run(OsStr::new("tar"), &args, &Spec { environment: Some(&environment), ..Spec::default() }, TREE_TIMEOUT)?;
    work.remove()
}
