//! Publishes `build/hamn`: build, sign and verify a candidate beside the
//! output, then replace the output atomically. Concurrent publications into
//! one directory are serialized, and only a complete, signed executable whose
//! `--version` matches the requested version is ever published. A failure at
//! any gate keeps the previous executable and removes the candidate.
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `build-host OUTPUT VERSION PROFILE`
pub fn run(args: &[String]) -> Result<(), String> {
    let [output, version, profile] = args else {
        return Err("usage: hamn-dev build-host OUTPUT VERSION PROFILE".into());
    };
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
    publish(&System, output.as_ref(), version, profile, &target)
}

/// The external steps of a publication, separated so tests can fail each.
trait Effects {
    fn build(&self, profile: &str, version: &str) -> Result<(), String>;
    fn sign(&self, candidate: &Path) -> Result<(), String>;
    fn check(&self, candidate: &Path) -> Result<(), String>;
    fn version(&self, candidate: &Path) -> Result<String, String>;
    fn replace(&self, candidate: &Path, output: &Path) -> Result<(), String>;
}

struct System;

impl Effects for System {
    fn build(&self, profile: &str, version: &str) -> Result<(), String> {
        run_checked(Command::new("cargo").args(["build", "--locked", "--profile", profile]).env("HAMN_VERSION", version))
    }

    fn sign(&self, candidate: &Path) -> Result<(), String> {
        run_checked(
            Command::new("codesign")
                .args(["--force", "--sign", "-", "--entitlements", "host/entitlements.plist"])
                .arg(candidate),
        )
    }

    fn check(&self, candidate: &Path) -> Result<(), String> {
        check_host_binary(candidate)
    }

    fn version(&self, candidate: &Path) -> Result<String, String> {
        capture(Command::new(candidate).arg("--version"))
    }

    fn replace(&self, candidate: &Path, output: &Path) -> Result<(), String> {
        fs::rename(candidate, output).map_err(|error| format!("{}: {error}", output.display()))
    }
}

fn publish(effects: &dyn Effects, output: &Path, version: &str, profile: &str, target: &Path) -> Result<(), String> {
    let output = std::path::absolute(output).map_err(|error| format!("{}: {error}", output.display()))?;
    let parent = output.parent().ok_or("OUTPUT has no parent directory")?;
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(parent.join(".hamn-publish.lock"))
        .map_err(|error| format!("publish lock: {error}"))?;
    // SAFETY: the descriptor is open for the call and flock only reads it.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!("publish lock: {}", io::Error::last_os_error()));
    }
    effects.build(profile, version)?;
    let source = target.join(if profile == "dev" { "debug" } else { profile }).join("hamn");
    let candidate = Candidate::create(parent)?;
    fs::copy(&source, candidate.path()).map_err(|error| format!("{}: {error}", source.display()))?;
    fs::set_permissions(candidate.path(), fs::Permissions::from_mode(0o755)).map_err(|error| error.to_string())?;
    effects.sign(candidate.path())?;
    effects.check(candidate.path())?;
    let actual = effects.version(candidate.path())?;
    if actual.trim() != format!("hamn {version}") {
        return Err(format!("linked executable reports {:?}, not hamn {version}", actual.trim()));
    }
    File::open(candidate.path()).and_then(|file| file.sync_all()).map_err(|error| error.to_string())?;
    effects.replace(candidate.path(), &output)?;
    File::open(parent).and_then(|directory| directory.sync_all()).map_err(|error| error.to_string())
}

/// A uniquely named file beside the output, removed unless it was published.
struct Candidate(PathBuf);

impl Candidate {
    fn create(parent: &Path) -> Result<Self, String> {
        for attempt in 0..100u32 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let path = parent.join(format!(".hamn-candidate-{}-{nanos:08x}{attempt:02x}", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
                Ok(_) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("{}: {error}", path.display())),
            }
        }
        Err("cannot create a unique candidate file".into())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        // After a successful rename nothing is left at this name.
        let _ = fs::remove_file(&self.0);
    }
}

/// `check-host-binary BINARY`: a signed Mach-O executable with an LC_UUID,
/// the virtualization entitlement, and runtime dependencies only in the OS.
pub fn check_host_binary(binary: &Path) -> Result<(), String> {
    let description = capture(Command::new("file").arg(binary))?;
    if !description.contains("Mach-O 64-bit executable") {
        return Err(format!("not a 64-bit Mach-O executable: {description}"));
    }
    run_checked(Command::new("codesign").args([OsStr::new("--verify"), OsStr::new("--strict"), binary.as_os_str()]))?;
    if !capture(Command::new("otool").arg("-l").arg(binary))?.contains("LC_UUID") {
        return Err("executable has no LC_UUID".into());
    }
    let libraries = capture(Command::new("otool").arg("-L").arg(binary))?;
    for dependency in libraries.lines().skip(1).filter_map(|line| line.split_whitespace().next()) {
        if !(dependency.starts_with("/usr/lib/") || dependency.starts_with("/System/Library/")) {
            return Err(format!("unexpected runtime dependency: {dependency}"));
        }
    }
    let entitlements = capture(Command::new("codesign").args(["-d", "--entitlements", ":-"]).arg(binary))?;
    if !entitlements.contains("com.apple.security.virtualization") {
        return Err("executable lacks the virtualization entitlement".into());
    }
    Ok(())
}

fn run_checked(command: &mut Command) -> Result<(), String> {
    let status = command.status().map_err(|error| format!("{command:?}: {error}"))?;
    if status.success() { Ok(()) } else { Err(format!("{command:?} failed: {status}")) }
}

fn capture(command: &mut Command) -> Result<String, String> {
    let output = command.output().map_err(|error| format!("{command:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{command:?} failed: {}: {}", output.status, String::from_utf8_lossy(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        failure: Option<&'static str>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl Fake {
        fn step(&self, name: &'static str) -> Result<(), String> {
            self.calls.borrow_mut().push(name);
            if self.failure == Some(name) { Err(format!("injected {name} failure")) } else { Ok(()) }
        }
    }

    impl Effects for Fake {
        fn build(&self, _: &str, _: &str) -> Result<(), String> {
            self.step("build")
        }
        fn sign(&self, _: &Path) -> Result<(), String> {
            self.step("sign")
        }
        fn check(&self, _: &Path) -> Result<(), String> {
            self.step("check")
        }
        fn version(&self, _: &Path) -> Result<String, String> {
            self.calls.borrow_mut().push("version");
            Ok(if self.failure == Some("version") { "hamn wrong".into() } else { "hamn 0.0.1\n".into() })
        }
        fn replace(&self, candidate: &Path, output: &Path) -> Result<(), String> {
            self.step("replace")?;
            System.replace(candidate, output)
        }
    }

    #[test]
    fn each_failed_gate_preserves_the_previous_executable_and_removes_the_candidate() {
        for failure in [Some("build"), Some("sign"), Some("check"), Some("version"), Some("replace"), None] {
            let directory = crate::support::tmp::TempDir::new("hamn-build-publish-");
            let root = directory.path();
            fs::create_dir_all(root.join("target/release")).unwrap();
            fs::write(root.join("target/release/hamn"), b"new executable").unwrap();
            fs::create_dir(root.join("build")).unwrap();
            let output = root.join("build/hamn");
            fs::write(&output, b"previous signed executable").unwrap();
            let fake = Fake { failure, calls: RefCell::new(Vec::new()) };
            let result = publish(&fake, &output, "0.0.1", "release", &root.join("target"));
            assert_eq!(result.is_err(), failure.is_some(), "{failure:?}: {result:?}");
            let expected: &[u8] = if failure.is_some() { b"previous signed executable" } else { b"new executable" };
            assert_eq!(fs::read(&output).unwrap(), expected, "{failure:?}");
            let leftovers: Vec<_> = fs::read_dir(root.join("build"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name.to_string_lossy().starts_with(".hamn-candidate-"))
                .collect();
            assert!(leftovers.is_empty(), "{failure:?}: {leftovers:?}");
            if failure.is_none() {
                assert_eq!(*fake.calls.borrow(), ["build", "sign", "check", "version", "replace"]);
            }
        }
    }
}
