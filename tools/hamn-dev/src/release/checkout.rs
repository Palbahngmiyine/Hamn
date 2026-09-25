//! What every release driver (`resolve-release`, `recover-release`,
//! `build-candidate`, `hosted-validation`, `gate`, `preflight-repository`,
//! `export-public-source`) shares: its environment inputs, the Hamn
//! checkout it runs in, and its output directory.
//!
//! Contract:
//! - The checkout is the Git top level of the working directory. A driver
//!   never reads or builds another tree.
//! - An unset variable reads as empty, like the `${NAME:-}` of the shell
//!   drivers these replace; a value that is not UTF-8 is an error.
//! - Every child (git, tar, make, uname, gh) runs with our environment plus
//!   `LC_ALL=C`, as the shell drivers exported it, and under a deadline
//!   that kills and reaps it (see [`super::process`]).
use super::process::{self, Output, Spec};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// No git query here is expected to take longer.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// A variable's value; unset is empty.
pub fn variable(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => Ok(String::new()),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid UTF-8")),
    }
}

/// The values of `names`, or `message` when any is empty.
pub fn required<const N: usize>(names: [&str; N], message: &str) -> Result<[String; N], String> {
    let mut values: [String; N] = std::array::from_fn(|_| String::new());
    for (value, name) in values.iter_mut().zip(names) {
        *value = variable(name)?;
        if value.is_empty() {
            return Err(message.to_owned());
        }
    }
    Ok(values)
}

/// Our environment plus `LC_ALL=C` and `overrides`, for a child's exact
/// environment. A variable that is not UTF-8 is an error, not dropped.
pub fn environment(overrides: &[(&str, &str)]) -> Result<BTreeMap<String, String>, String> {
    let mut environment = BTreeMap::new();
    for (name, value) in std::env::vars_os() {
        let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
            return Err(format!("environment variable {} is not valid UTF-8", name.to_string_lossy()));
        };
        environment.insert(name.to_owned(), value.to_owned());
    }
    for (name, value) in [("LC_ALL", "C")].iter().chain(overrides) {
        environment.insert((*name).to_owned(), (*value).to_owned());
    }
    Ok(environment)
}

/// Runs a tool that must succeed and returns its standard output.
pub fn tool<S: AsRef<OsStr>>(program: &str, args: &[S], timeout: Duration) -> Result<String, String> {
    let environment = environment(&[])?;
    process::run(OsStr::new(program), args, &Spec { environment: Some(&environment), ..Spec::default() }, timeout)
}

/// `uname -m`, looked up on PATH as the shell drivers did.
pub fn machine() -> Result<String, String> {
    Ok(tool("uname", &["-m"], Duration::from_secs(30))?.trim_end_matches('\n').to_owned())
}

/// `[1-9][0-9]*`
pub fn is_positive_decimal(text: &str) -> bool {
    text.bytes().next().is_some_and(|first| first != b'0') && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// `path` if it names an existing directory that is not a link; `message`
/// otherwise.
pub fn existing_directory<'a>(path: &'a str, message: &str) -> Result<&'a Path, String> {
    let directory = fs::symlink_metadata(path).is_ok_and(|info| info.file_type().is_dir());
    if directory { Ok(Path::new(path)) } else { Err(message.to_owned()) }
}

/// Creates `OUTPUT_DIR` with its parents like `mkdir -p`; it must then be a
/// directory, not a link, with no entries.
pub fn empty_output_directory(path: &str) -> Result<&Path, String> {
    fs::create_dir_all(path).map_err(|error| format!("cannot create OUTPUT_DIR {path}: {error}"))?;
    let directory = existing_directory(path, "OUTPUT_DIR is unsafe")?;
    let mut entries = fs::read_dir(directory).map_err(|error| format!("{path}: {error}"))?;
    if entries.next().is_some() {
        return Err("OUTPUT_DIR must be empty".into());
    }
    Ok(directory)
}

/// The GitHub step output file: an existing regular file, not a link.
pub fn github_output() -> Result<PathBuf, String> {
    let path = variable("GITHUB_OUTPUT")?;
    let regular = !path.is_empty() && fs::symlink_metadata(&path).is_ok_and(|info| info.file_type().is_file());
    if regular { Ok(PathBuf::from(path)) } else { Err("GITHUB_OUTPUT must name an existing regular file".into()) }
}

/// Runs `git -C DIRECTORY ARGS` with `overrides` added to its environment.
pub fn git_in(
    directory: &Path,
    args: &[&str],
    overrides: &[(&str, &str)],
    timeout: Duration,
) -> Result<Output, String> {
    let mut words: Vec<OsString> = vec!["-C".into(), directory.into()];
    words.extend(args.iter().map(OsString::from));
    let environment = environment(overrides)?;
    process::capture(OsStr::new("git"), &words, &Spec { environment: Some(&environment), ..Spec::default() }, timeout)
}

/// The standard output of a git command that must succeed, without its
/// trailing newlines (as `$(git ...)` reads it).
pub fn git_text(directory: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let output = git_in(directory, args, &[], timeout)?;
    if !output.status.success() {
        return Err(format!("git {} failed: {:?}: {}", args.join(" "), output.code(), output.stderr_lossy().trim()));
    }
    Ok(output.stdout_text()?.trim_end_matches('\n').to_owned())
}

/// The Hamn checkout the working directory is in.
pub struct Checkout {
    pub root: PathBuf,
}

impl Checkout {
    pub fn current() -> Result<Self, String> {
        let root = git_text(Path::new("."), &["rev-parse", "--show-toplevel"], GIT_TIMEOUT)
            .map_err(|error| format!("the working directory is not a Git checkout: {error}"))?;
        Ok(Self { root: PathBuf::from(root) })
    }

    pub fn git(&self, args: &[&str]) -> Result<Output, String> {
        git_in(&self.root, args, &[], GIT_TIMEOUT)
    }

    pub fn git_text(&self, args: &[&str]) -> Result<String, String> {
        git_text(&self.root, args, GIT_TIMEOUT)
    }

    /// The checked-out commit.
    pub fn head(&self) -> Result<String, String> {
        self.git_text(&["rev-parse", "--verify", "HEAD"])
    }

    /// The commit `reference` names.
    pub fn commit(&self, reference: &str) -> Result<String, String> {
        self.git_text(&["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
    }

    pub fn tree(&self, commit: &str) -> Result<String, String> {
        self.git_text(&["rev-parse", &format!("{commit}^{{tree}}")])
    }

    /// Whether `git status --porcelain` lists anything: a change, or an
    /// untracked file that is not ignored.
    pub fn is_dirty(&self) -> Result<bool, String> {
        Ok(!self.git_text(&["status", "--porcelain"])?.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::tmp::TempDir;

    #[test]
    fn positive_decimals_have_no_sign_or_leading_zero() {
        for accepted in ["1", "9", "10", "417123456"] {
            assert!(is_positive_decimal(accepted), "{accepted}");
        }
        for rejected in ["", "0", "01", "-1", "+1", "1.0", " 1", "1 ", "local", "１"] {
            assert!(!is_positive_decimal(rejected), "{rejected:?}");
        }
    }

    #[test]
    fn output_directory_is_created_and_must_be_empty_and_unlinked() {
        let directory = TempDir::new("hamn-release-checkout-");
        let text = |path: &Path| path.to_str().unwrap().to_owned();
        let nested = directory.path().join("a/b");
        assert_eq!(empty_output_directory(&text(&nested)).unwrap(), nested);
        fs::write(nested.join("entry"), "").unwrap();
        assert_eq!(empty_output_directory(&text(&nested)).unwrap_err(), "OUTPUT_DIR must be empty");
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(directory.path().join("a"), &link).unwrap();
        assert_eq!(empty_output_directory(&text(&link)).unwrap_err(), "OUTPUT_DIR is unsafe");
        let file = directory.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(empty_output_directory(&text(&file)).unwrap_err().starts_with("cannot create OUTPUT_DIR"));
        assert!(existing_directory(&text(&link), "unsafe").is_err());
        assert!(existing_directory(&text(&directory.path().join("missing")), "unsafe").is_err());
        assert!(existing_directory(&text(directory.path()), "unsafe").is_ok());
    }
}
