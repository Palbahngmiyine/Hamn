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

/// `[1-9][0-9]*`
pub fn is_positive_decimal(text: &str) -> bool {
    text.bytes().next().is_some_and(|first| first != b'0') && text.bytes().all(|byte| byte.is_ascii_digit())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_decimals_have_no_sign_or_leading_zero() {
        for accepted in ["1", "9", "10", "417123456"] {
            assert!(is_positive_decimal(accepted), "{accepted}");
        }
        for rejected in ["", "0", "01", "-1", "+1", "1.0", " 1", "1 ", "local", "１"] {
            assert!(!is_positive_decimal(rejected), "{rejected:?}");
        }
    }
}
