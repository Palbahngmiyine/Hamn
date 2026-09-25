//! Generation collection by the native installer (`hamn __install-support
//! install`) and `hamn __install-support prune`, with every write inside a
//! disposable root: bounded generations, foreign and symlinked entries,
//! recovery references from other homes, the shared transaction lock,
//! running executables, an unavailable process scan and interrupted
//! retirement.
use crate::runner::{self, case};
use crate::support::real_cli::Reaped;
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, digest};
use crate::support::{hamn, pty};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "generation-cleanup",
        "bounded generations, ownership, recovery roots, running executable and retired retry",
        vec![case("generations_are_bounded_and_collected_only_when_unreferenced", generations_are_bounded_and_collected_only_when_unreferenced)],
        filters,
    )
}

/// The generation-cleanup fixture: a native executable that reports ready
/// and then waits, so a test can hold an installed generation open.
pub fn waiting_executable(_program: &str, _args: &[String]) -> ExitCode {
    println!("ready");
    std::io::stdout().flush().expect("flush ready");
    loop {
        // SAFETY: pause only waits for a signal.
        unsafe { libc::pause() };
    }
}

/// The lock-holder fixture: takes an exclusive lock on each argument path
/// (as any installer or updater transaction does), reports ready, and holds
/// the locks until it is killed.
pub fn lock_holder(_program: &str, args: &[String]) -> ExitCode {
    let mut held = Vec::new();
    for path in args {
        let file = fs::OpenOptions::new().append(true).open(path).unwrap_or_else(|error| panic!("{path}: {error}"));
        // SAFETY: flock locks this fixture's own open file description.
        assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0, "lock {path}");
        held.push(file);
    }
    println!("ready");
    std::io::stdout().flush().expect("flush ready");
    loop {
        // SAFETY: pause only waits for a signal.
        unsafe { libc::pause() };
    }
}

const RUN: Duration = Duration::from_secs(60);

struct Roots {
    root: PathBuf,
    bindir: PathBuf,
    datadir: PathBuf,
    home: PathBuf,
}

impl Roots {
    fn installer(&self, source: &Path, bindir: &Path, datadir: &Path) -> Command {
        upgrade::install_command(&hamn(), source, bindir, datadir, &self.home)
    }

    /// Installs `source` and returns the new active generation.
    fn install(&self, source: &Path) -> PathBuf {
        let result = upgrade::run(&mut self.installer(source, &self.bindir, &self.datadir), RUN);
        assert_eq!(result.returncode, 0, "{}", result.stderr());
        self.active_generation()
    }

    fn active_generation(&self) -> PathBuf {
        let target = fs::read_link(self.bindir.join("hamn")).unwrap();
        target.parent().and_then(Path::parent).unwrap().to_path_buf()
    }

    /// `hamn __install-support prune BINDIR DATADIR [KEEP]`, which takes both
    /// transaction locks as any installer does.
    fn prune(&self, keep: Option<&str>, sandbox: Option<&Path>) -> upgrade::Output {
        let mut command = match sandbox {
            Some(profile) => {
                let mut command = Command::new("/usr/bin/sandbox-exec");
                command.arg("-f").arg(profile).arg(hamn());
                command
            }
            None => Command::new(hamn()),
        };
        command.args(["__install-support", "prune"]).arg(&self.bindir).arg(&self.datadir).args(keep).env("HOME", &self.home);
        upgrade::run(&mut command, RUN)
    }

    fn collect(&self, keep: Option<&str>) {
        let result = self.prune(keep, None);
        assert_eq!(result.returncode, 0, "{}", result.stderr());
    }

    fn generation_count(&self) -> usize {
        fs::read_dir(self.datadir.join(".hamn-generations")).unwrap().count()
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    let status = Command::new("/bin/cp").arg("-Rp").arg(source).arg(destination).status().expect("cp");
    assert!(status.success(), "copy {}", source.display());
}

fn generations_are_bounded_and_collected_only_when_unreferenced() {
    let directory = TempDir::new("hamn-generation-test-");
    let root = fs::canonicalize(directory.path()).unwrap();
    let roots = Roots { bindir: root.join("bin"), datadir: root.join("data"), home: root.join("home"), root };
    fs::create_dir(&roots.home).unwrap();
    let binary = hamn();

    // A foreign/symlinked transaction lock must fail before it is opened.
    let (bad_bin, bad_data) = (roots.root.join("bad-bin"), roots.root.join("bad-data"));
    fs::create_dir(&bad_bin).unwrap();
    let sentinel = roots.root.join("lock-sentinel");
    fs::write(&sentinel, "keep").unwrap();
    std::os::unix::fs::symlink(&sentinel, bad_bin.join(".hamn-transaction.lock")).unwrap();
    let rejected = upgrade::run(&mut roots.installer(&binary, &bad_bin, &bad_data), RUN);
    assert_ne!(rejected.returncode, 0);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
    assert!(!bad_data.exists());

    let first = roots.install(&binary);
    let second = roots.install(&binary);
    let third = roots.install(&binary);
    assert!(!first.exists(), "repeated installs accumulated obsolete generations");
    assert!(second.exists() && third.exists(), "active/predecessor removed");
    assert_eq!(roots.generation_count(), 2);
    roots.collect(Some(third.join("bin/hamn").to_str().unwrap()));
    assert!(second.exists(), "unchanged release collection removed predecessor");

    // Unknown directories and symlinks are never adopted.
    let generations = roots.datadir.join(".hamn-generations");
    let foreign = generations.join(format!("{}-ABC123", "a".repeat(64)));
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("sentinel"), "foreign").unwrap();
    let outside = roots.root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), "keep").unwrap();
    let link = generations.join(format!("{}-ABC123", "b".repeat(64)));
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let fourth = roots.install(&binary);
    assert!(foreign.exists() && fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(outside.join("sentinel")).unwrap(), "keep");

    recovery_metadata_preserves_generations(&roots, &binary, &third, &fourth);
    transaction_lock_spans_recovery_and_publication(&roots, &binary);
    running_executable_is_kept(&roots, &binary);
    earlier_layout_generation_is_never_collected(&roots);

    // An owned, unreferenced generation is collected without the retired
    // pre-0.1.2 `.hamn-retention` opt-in marker (never written any more).
    let current = roots.active_generation();
    assert!(!current.join(".hamn-retention").exists());
    let name = current.file_name().unwrap().to_str().unwrap();
    let unmarked = current.with_file_name(format!("{}-UNMARK", &name[..64]));
    copy_tree(&current, &unmarked);
    roots.collect(None);
    assert!(!unmarked.exists());

    // An invalid predecessor reference fails collection loudly.
    let previous_record = current.join(".hamn-previous-target");
    let saved_previous = fs::read(&previous_record).unwrap();
    fs::write(&previous_record, "").unwrap();
    let invalid = roots.prune(None, None);
    assert_ne!(invalid.returncode, 0);
    assert!(invalid.stderr().contains("invalid predecessor reference"), "{}", invalid.stderr());
    fs::write(&previous_record, saved_previous).unwrap();

    // An unavailable process scan must preserve a collectible generation.
    let candidate = roots.install(&binary);
    let name = candidate.file_name().unwrap().to_str().unwrap();
    let spare = candidate.with_file_name(format!("{}-FAULT1", &name[..64]));
    copy_tree(&candidate, &spare);
    let profile = roots.root.join("deny-scanner.sb");
    fs::write(&profile, "(version 1)(allow default)(deny process-exec (literal \"/usr/sbin/lsof\"))\n").unwrap();
    let failed = roots.prune(None, Some(&profile));
    assert_ne!(failed.returncode, 0);
    assert!(spare.exists());
    roots.collect(None);
    assert!(!spare.exists());

    // Interrupted retirement is retryable even after the binary has gone.
    let previous = fs::read_to_string(candidate.join(".hamn-previous-target")).unwrap();
    let old = Path::new(previous.trim_end()).parent().and_then(Path::parent).unwrap().to_path_buf();
    let retired = old.with_file_name(format!(".retired-{}", old.file_name().unwrap().to_str().unwrap()));
    fs::rename(&old, &retired).unwrap();
    fs::remove_dir_all(retired.join("bin")).unwrap();
    roots.collect(None);
    assert!(!retired.exists());
}

/// Recovery metadata preserves every generation until it can be
/// interpreted safely; a different HOME is remembered by the generation's
/// recovery root, and an inaccessible recovery root counts as pending.
fn recovery_metadata_preserves_generations(roots: &Roots, binary: &Path, third: &Path, fourth: &Path) {
    let cache = roots.home.join(".hamn/cache");
    fs::create_dir_all(&cache).unwrap();
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
    let journal = cache.join(".hamn-update-transaction");
    fs::DirBuilder::new().mode(0o700).create(&journal).unwrap();
    roots.install(binary);
    assert!(third.exists());
    fs::remove_dir(&journal).unwrap();
    let other_cache = roots.root.join("other-home/cache");
    fs::create_dir_all(&other_cache).unwrap();
    let other_journal = other_cache.join(".hamn-update-transaction");
    fs::create_dir(&other_journal).unwrap();
    let other_text = other_cache.to_str().unwrap();
    fs::write(third.join(format!(".hamn-recovery-root-{}", digest(other_text.as_bytes()))), other_text).unwrap();
    roots.install(binary);
    assert!(third.exists() && !fourth.exists());
    for blocked in [other_cache.parent().unwrap(), other_cache.as_path()] {
        fs::set_permissions(blocked, fs::Permissions::from_mode(0o000)).unwrap();
        let result = upgrade::run(&mut roots.installer(binary, &roots.bindir, &roots.datadir), RUN);
        fs::set_permissions(blocked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result.returncode, 0, "{}", result.stderr());
        assert!(third.exists(), "inaccessible recovery state was treated as absent");
    }
    fs::remove_dir(&other_journal).unwrap();
    roots.install(binary);
    assert!(!third.exists());
}

/// The same transaction locks span updater recovery and installer
/// publication: an installer waits for a holder, and SIGKILL of the holder
/// releases the locks without stale-lock cleanup.
fn transaction_lock_spans_recovery_and_publication(roots: &Roots, binary: &Path) {
    let active = roots.active_generation();
    let holder_program = roots.root.join("lock-holder");
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &holder_program).unwrap();
    let holder = Command::new(&holder_program)
        .arg(roots.bindir.join(".hamn-transaction.lock"))
        .arg(roots.root.join(".data.hamn-transaction.lock"))
        .env("HAMN_DEV_FIXTURE", "generation-lock-holder")
        .stdout(Stdio::piped())
        .spawn()
        .expect("lock holder");
    let mut holder = Reaped(Some(holder));
    let stdout = holder.0.as_mut().unwrap().stdout.take().unwrap();
    assert!(!pty::readable(&[stdout.as_raw_fd()], Duration::from_secs(5)).is_empty(), "lock holder never became ready");
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");
    let mut installer = roots.installer(binary, &roots.bindir, &roots.datadir);
    let waiting = installer.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("waiting installer");
    let mut waiting = Reaped(Some(waiting));
    let waiting_fd = waiting.0.as_ref().unwrap().stdout.as_ref().unwrap().as_raw_fd();
    // A negative observation needs a window: the installer prints nothing
    // while the holder owns the lock.
    assert!(pty::readable(&[waiting_fd], Duration::from_secs(1)).is_empty(), "installer ran while the lock was held");
    assert!(waiting.0.as_mut().unwrap().try_wait().unwrap().is_none());
    assert_eq!(roots.active_generation(), active);
    let mut holder_child = holder.0.take().unwrap();
    holder_child.kill().unwrap();
    holder_child.wait().unwrap();
    let result = crate::support::real_cli::communicate(waiting.0.take().unwrap(), RUN);
    assert_eq!(result.returncode, 0, "{}", result.stderr());
}

/// A real native executable stays alive across more than two
/// installations; its generation is kept until it exits.
fn running_executable_is_kept(roots: &Roots, binary: &Path) {
    let sleeper = roots.root.join("wait");
    fs::copy(std::env::current_exe().unwrap(), &sleeper).unwrap();
    let running = roots.install(&sleeper);
    let process = Command::new(running.join("bin/hamn"))
        .env("HAMN_DEV_FIXTURE", "generation-wait")
        .stdout(Stdio::piped())
        .spawn()
        .expect("installed waiting executable");
    let mut process = Reaped(Some(process));
    let stdout = process.0.as_mut().unwrap().stdout.take().unwrap();
    assert!(!pty::readable(&[stdout.as_raw_fd()], Duration::from_secs(5)).is_empty(), "waiting executable never started");
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");
    roots.install(binary);
    roots.install(binary);
    assert!(running.exists());
    assert!(process.0.as_mut().unwrap().try_wait().unwrap().is_none());
    let mut child = process.0.take().unwrap();
    pty::kill(child.id(), libc::SIGTERM);
    child.wait().unwrap();
    roots.install(binary);
    assert!(!running.exists(), "an exited executable was never collected");
}

/// A generation of the earlier layout (version 1 marker, `share/hamn/src`)
/// is of unknown ownership to this Hamn: collection never removes it.
fn earlier_layout_generation_is_never_collected(roots: &Roots) {
    let current = roots.active_generation();
    let name = current.file_name().unwrap().to_str().unwrap();
    let earlier = current.with_file_name(format!("{}-EARLY1", &name[..64]));
    copy_tree(&current, &earlier);
    let marker = earlier.join(".hamn-generation");
    let text = fs::read_to_string(&marker).unwrap();
    assert!(text.starts_with("version=2\n"), "{text}");
    fs::write(&marker, text.replacen("version=2", "version=1", 1)).unwrap();
    fs::create_dir_all(earlier.join("share/hamn/src/packaging/release")).unwrap();
    roots.collect(None);
    assert!(earlier.join("bin/hamn").is_file(), "an earlier-layout generation was collected");
    fs::remove_dir_all(&earlier).unwrap();
}
