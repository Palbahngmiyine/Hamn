//! The native host installer (`hamn __install-support install`, which
//! `make install` runs): immutable generations, ownership refusals that
//! change nothing, private roots, SIGKILL at the one externally visible
//! commit (the command link rename), serialized concurrent installers and
//! the `make install` recipe itself. Every write is inside a disposable
//! root with its own HOME; no VM, network or shared install is touched.
//! Test barriers name the commit point, and every FIFO wait is bounded by
//! an explicit deadline.
use crate::runner::{self, case};
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, Barrier, Group, file_digest};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "host-install",
        "immutable host install ownership and atomic publish",
        vec![
            case("symlinked_install_lock_is_refused_before_open", symlinked_install_lock_is_refused_before_open),
            case(
                "fresh_install_with_spaces_publishes_a_generation_without_docker",
                fresh_install_with_spaces_publishes_a_generation_without_docker,
            ),
            case("pre_generation_installs_are_refused_unchanged", pre_generation_installs_are_refused_unchanged),
            case(
                "unverifiable_released_generation_is_refused_unchanged",
                unverifiable_released_generation_is_refused_unchanged,
            ),
            case(
                "foreign_unmanaged_and_tampered_paths_are_preserved",
                foreign_unmanaged_and_tampered_paths_are_preserved,
            ),
            case("group_writable_roots_are_refused", group_writable_roots_are_refused),
            case(
                "kill_before_first_publication_leaves_a_collectible_generation",
                kill_before_first_publication_leaves_a_collectible_generation,
            ),
            case(
                "kill_before_or_after_the_link_rename_keeps_one_complete_target",
                kill_before_or_after_the_link_rename_keeps_one_complete_target,
            ),
            case("concurrent_installers_serialize_before_staging", concurrent_installers_serialize_before_staging),
            case("overlapping_roots_are_refused", overlapping_roots_are_refused),
            case("make_install_publishes_the_built_executable", make_install_publishes_the_built_executable),
        ],
        filters,
    )
}

const RUN: Duration = Duration::from_secs(60);

/// A disposable work root (canonical) with a private HOME.
struct Work {
    root: PathBuf,
    home: PathBuf,
    hamn: PathBuf,
    _directory: TempDir,
}

impl Work {
    fn new() -> Self {
        let directory = TempDir::new("hamn-install-");
        let root = fs::canonicalize(directory.path()).unwrap();
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        Self { home, hamn: crate::support::hamn(), root, _directory: directory }
    }

    fn command(&self, source: &Path, bindir: &Path, datadir: &Path) -> Command {
        upgrade::install_command(&self.hamn, source, bindir, datadir, &self.home)
    }

    fn install(&self, source: &Path, bindir: &Path, datadir: &Path) -> upgrade::Output {
        upgrade::install(&self.hamn, source, bindir, datadir, &self.home)
    }

    /// An install that must fail; returns its standard error.
    fn refused(&self, source: &Path, bindir: &Path, datadir: &Path, what: &str) -> String {
        let result = upgrade::run(&mut self.command(source, bindir, datadir), RUN);
        assert_ne!(result.returncode, 0, "the installer accepted {what}: {}", result.stdout());
        result.stderr()
    }

    /// A copy of the installed executable with extra trailing bytes, so its
    /// digest (and generation name) differs.
    fn replacement(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        let mut bytes = fs::read(&self.hamn).unwrap();
        bytes.extend_from_slice(format!("{name}\n").as_bytes());
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

fn canonical_data(datadir: &Path) -> PathBuf {
    fs::canonicalize(datadir.parent().unwrap()).unwrap().join(datadir.file_name().unwrap())
}

/// The documented managed layout for `source`: a link into a marked
/// version-2 generation of exactly its bytes, the data marker, and no
/// Docker command or earlier-layout scripts.
fn assert_managed_install(bindir: &Path, datadir: &Path, source: &Path) {
    let link = bindir.join("hamn");
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert!(fs::symlink_metadata(bindir.join("docker")).is_err(), "Hamn installed or replaced a Docker CLI command");
    let target = fs::read_link(&link).unwrap();
    let generations = canonical_data(datadir).join(".hamn-generations");
    let relative = target.strip_prefix(&generations).expect("hamn does not target its canonical generation");
    assert!(relative.ends_with("bin/hamn") && relative.components().count() == 3, "{}", target.display());
    let generation = upgrade::generation_of(&target);
    assert!(fs::symlink_metadata(&generation).unwrap().is_dir());
    let marker = generation.join(".hamn-generation");
    let info = fs::symlink_metadata(&marker).unwrap();
    assert_eq!((info.mode() & 0o7777, info.nlink()), (0o600, 1));
    let expected = file_digest(source);
    let text = fs::read_to_string(&marker).unwrap();
    assert!(text.starts_with("version=2\n") && text.contains(&format!("\nbinary_sha256={expected}\n")), "{text}");
    assert_eq!(file_digest(&target), expected);
    assert_eq!(fs::read(&target).unwrap(), fs::read(source).unwrap());
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o755);
    // A source build carries no manifest pointer, scripts or guest sources.
    for absent in ["share", "share/hamn/src", "guest", "vendor"] {
        assert!(fs::symlink_metadata(generation.join(absent)).is_err(), "{absent}");
    }
    assert_eq!(fs::read_to_string(datadir.join(".hamn-managed")).unwrap(), "version=1\n");
}

fn symlinked_install_lock_is_refused_before_open() {
    let work = Work::new();
    let (bindir, datadir) = (work.root.join("lock-bin"), work.root.join("lock-share/hamn/src"));
    fs::create_dir_all(&bindir).unwrap();
    fs::create_dir_all(datadir.parent().unwrap()).unwrap();
    let target = work.root.join("lock-target");
    fs::write(&target, "keep-lock\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(&target, bindir.join(".hamn-install.lock")).unwrap();
    let stderr = work.refused(&work.hamn, &bindir, &datadir, "a symlinked lock");
    assert!(stderr.contains("refusing unsafe install lock path"), "{stderr}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep-lock\n");
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o644);
    assert!(!datadir.exists());
}

fn fresh_install_with_spaces_publishes_a_generation_without_docker() {
    let work = Work::new();
    let (bindir, datadir) = (work.root.join("bin with space"), work.root.join("share with space/hamn/src"));
    let installed = work.install(&work.hamn, &bindir, &datadir);
    assert_managed_install(&bindir, &datadir, &work.hamn);
    let stdout = installed.stdout();
    assert!(stdout.contains(&format!("installed: {}/hamn -> ", bindir.display())), "{stdout}");
    assert!(stdout.contains("verify: hamn --headless vm start --profile default --yes"), "{stdout}");
    let status =
        upgrade::run(Command::new(bindir.join("hamn")).args(["--headless", "vm", "list"]).env("HOME", &work.home), RUN);
    assert!(status.stdout().contains("\"data\":[]"), "{}", status.stdout());
    assert!(!work.home.join(".hamn").exists(), "a read-only query created runtime state");
    // Reinstall succeeds without mutating or deleting the previous generation.
    let old_target = fs::read_link(bindir.join("hamn")).unwrap();
    let old_bytes = fs::read(&old_target).unwrap();
    work.install(&work.hamn, &bindir, &datadir);
    assert_managed_install(&bindir, &datadir, &work.hamn);
    assert_ne!(fs::read_link(bindir.join("hamn")).unwrap(), old_target);
    assert_eq!(fs::read(&old_target).unwrap(), old_bytes);
}

/// Every path, type, mode and file digest below each root, except the
/// permanent lock files that any installer run creates before its checks.
type Tree = BTreeMap<PathBuf, (u32, bool, Option<String>)>;

fn tree_state(roots: &[&Path]) -> Tree {
    let mut tree = Tree::new();
    for root in roots {
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(&path).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name.ends_with(".hamn-install.lock") || name.ends_with(".hamn-transaction.lock") {
                    continue;
                }
                let info = fs::symlink_metadata(&path).unwrap();
                if info.is_dir() {
                    pending.push(path.clone());
                }
                let content = info.is_file().then(|| file_digest(&path));
                tree.insert(path, (info.mode() & 0o7777, info.file_type().is_symlink(), content));
            }
        }
    }
    tree
}

/// An install that must be refused with `message`, change nothing, and
/// never execute a standalone `hamn` (a spoof records its execution).
fn assert_refused_unchanged(work: &Work, bindir: &Path, datadir: &Path, message: &str, label: &str) {
    let before = tree_state(&[bindir, datadir]);
    let sentinel = work.root.join("legacy-spoof-ran");
    let result = upgrade::run(
        work.command(&work.hamn, bindir, datadir).env("SPOOF_SENTINEL", &sentinel).env("HAMN_ADOPT_LEGACY", "1"),
        RUN,
    );
    assert_ne!(result.returncode, 0, "the installer adopted a pre-generation install ({label})");
    assert!(result.stderr().contains(message), "refusal is not explained ({label}): {}", result.stderr());
    assert_eq!(tree_state(&[bindir, datadir]), before, "refusing changed the installation ({label})");
    assert!(!sentinel.exists(), "the installer executed a standalone hamn ({label})");
}

fn pre_generation_installs_are_refused_unchanged() {
    let work = Work::new();
    let (bindir, datadir) = (work.root.join("legacy-bin"), work.root.join("legacy-share/hamn/src"));
    for directory in [bindir.clone(), datadir.join("guest"), datadir.join("vendor")] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::copy(&work.hamn, bindir.join("hamn")).unwrap();
    fs::set_permissions(bindir.join("hamn"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(bindir.join(".hamn-binary.sha256"), format!("sha256 {}\n", file_digest(&bindir.join("hamn")))).unwrap();
    fs::write(datadir.join(".hamn-managed"), "").unwrap();
    fs::write(datadir.join("guest/sentinel"), "keep-legacy-guest\n").unwrap();
    fs::write(datadir.join("vendor/sentinel"), "keep-legacy-vendor\n").unwrap();
    let canonical = canonical_data(&datadir);
    assert_refused_unchanged(
        &work,
        &bindir,
        &datadir,
        &format!(
            "is a pre-release Hamn install (empty data marker), which this installer no longer migrates; move {} aside and install again",
            canonical.display()
        ),
        "legacy-empty-marker",
    );
    // A versioned data marker does not make the binary marker acceptable.
    fs::write(datadir.join(".hamn-managed"), "version=1\n").unwrap();
    assert_refused_unchanged(
        &work,
        &bindir,
        &datadir,
        ".hamn-binary.sha256 marks a pre-release Hamn install, which this installer no longer migrates; move it and",
        "legacy-binary-marker",
    );
    // Moving the standalone executable and its marker aside, as advised,
    // allows a fresh managed install that keeps the old source data.
    fs::rename(bindir.join("hamn"), work.root.join("moved-hamn")).unwrap();
    fs::rename(bindir.join(".hamn-binary.sha256"), work.root.join("moved-marker")).unwrap();
    work.install(&work.hamn, &bindir, &datadir);
    assert_managed_install(&bindir, &datadir, &work.hamn);
    assert_eq!(fs::read_to_string(datadir.join("guest/sentinel")).unwrap(), "keep-legacy-guest\n");
    assert_eq!(fs::read_to_string(datadir.join("vendor/sentinel")).unwrap(), "keep-legacy-vendor\n");

    // An unmarked standalone executable (a script that would record its
    // execution, then a hardlinked copy) is refused, not run or adopted.
    let (standalone_bin, standalone_data) =
        (work.root.join("standalone-bin"), work.root.join("standalone-share/hamn/src"));
    fs::create_dir_all(&standalone_bin).unwrap();
    fs::create_dir_all(&standalone_data).unwrap();
    upgrade::write_executable(&standalone_bin.join("hamn"), "#!/bin/sh\ntouch \"$SPOOF_SENTINEL\"\nexit 0\n");
    fs::write(standalone_data.join(".hamn-managed"), "version=1\n").unwrap();
    let canonical_bin = fs::canonicalize(&standalone_bin).unwrap();
    assert_refused_unchanged(
        &work,
        &standalone_bin,
        &standalone_data,
        &format!(
            "refusing to replace {}/hamn: it is not a managed Hamn generation link (an older standalone Hamn or another program); move it aside and install again",
            canonical_bin.display()
        ),
        "standalone-script",
    );
    fs::remove_file(standalone_bin.join("hamn")).unwrap();
    fs::copy(&work.hamn, standalone_bin.join("hamn")).unwrap();
    fs::set_permissions(standalone_bin.join("hamn"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::hard_link(standalone_bin.join("hamn"), work.root.join("standalone-hardlink")).unwrap();
    assert_refused_unchanged(
        &work,
        &standalone_bin,
        &standalone_data,
        "it is not a managed Hamn generation link",
        "standalone-hardlink",
    );

    // An empty data marker without any executable is refused as well.
    let (empty_bin, empty_data) = (work.root.join("empty-marker-bin"), work.root.join("empty-marker-share/hamn/src"));
    fs::create_dir_all(&empty_bin).unwrap();
    fs::create_dir_all(&empty_data).unwrap();
    fs::write(empty_data.join(".hamn-managed"), "").unwrap();
    assert_refused_unchanged(
        &work,
        &empty_bin,
        &empty_data,
        "is a pre-release Hamn install (empty data marker)",
        "empty-marker-only",
    );
}

/// A generation marked as the Hamn 0.1.x layout (version 1 marker) that
/// fails the checks its installer applied (here: no `share/hamn/src`
/// scripts) is not migrated: the refusal says what to move aside and to
/// reinstall with install.sh, and changes nothing. The `released-migration`
/// suite migrates genuine 0.1.2 installs.
fn unverifiable_released_generation_is_refused_unchanged() {
    let work = Work::new();
    let (bindir, datadir) = (work.root.join("earlier-bin"), work.root.join("earlier-share/hamn/src"));
    work.install(&work.hamn, &bindir, &datadir);
    let target = fs::read_link(bindir.join("hamn")).unwrap();
    let generation = upgrade::generation_of(&target);
    let marker = generation.join(".hamn-generation");
    let text = fs::read_to_string(&marker).unwrap();
    fs::write(&marker, text.replacen("version=2", "version=1", 1)).unwrap();
    fs::create_dir_all(generation.join("share/hamn/src/packaging/release")).unwrap();
    let message = format!(
        "{}/hamn points to a Hamn 0.1.x generation that fails the ownership checks it was installed with, \
         so this Hamn cannot migrate it; move {}/hamn and {} aside, then reinstall with install.sh",
        fs::canonicalize(&bindir).unwrap().display(),
        fs::canonicalize(&bindir).unwrap().display(),
        canonical_data(&datadir).display()
    );
    assert_refused_unchanged(&work, &bindir, &datadir, &message, "earlier-layout");
    assert_eq!(fs::read_link(bindir.join("hamn")).unwrap(), target);
}

fn foreign_unmanaged_and_tampered_paths_are_preserved() {
    let work = Work::new();
    // Existing Docker commands are outside Hamn ownership and remain.
    let (docker_bin, docker_data) =
        (work.root.join("foreign-docker-bin"), work.root.join("foreign-docker-share/hamn/src"));
    fs::create_dir_all(&docker_bin).unwrap();
    upgrade::write_executable(&docker_bin.join("docker"), "keep-docker\n");
    work.install(&work.hamn, &docker_bin, &docker_data);
    assert!(fs::symlink_metadata(docker_bin.join("hamn")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(docker_bin.join("docker")).unwrap(), "keep-docker\n");

    let unmanaged = work.root.join("unmanaged-share/hamn/src");
    fs::create_dir_all(&unmanaged).unwrap();
    fs::write(unmanaged.join("foreign"), "keep-data\n").unwrap();
    let stderr = work.refused(&work.hamn, &work.root.join("unmanaged-bin"), &unmanaged, "unmanaged data");
    assert!(stderr.contains("refusing to modify unmanaged data directory"), "{stderr}");
    assert_eq!(fs::read_to_string(unmanaged.join("foreign")).unwrap(), "keep-data\n");

    let (link_bin, link_data) = (work.root.join("foreign-link-bin"), work.root.join("foreign-link-share/hamn/src"));
    fs::create_dir_all(&link_bin).unwrap();
    fs::create_dir_all(&link_data).unwrap();
    upgrade::write_executable(&work.root.join("foreign-hamn"), "foreign\n");
    std::os::unix::fs::symlink(work.root.join("foreign-hamn"), link_bin.join("hamn")).unwrap();
    fs::write(link_data.join(".hamn-managed"), "version=1\n").unwrap();
    let stderr = work.refused(&work.hamn, &link_bin, &link_data, "a foreign hamn symlink");
    assert!(stderr.contains("refusing foreign hamn symlink"), "{stderr}");
    assert_eq!(fs::read_link(link_bin.join("hamn")).unwrap(), work.root.join("foreign-hamn"));

    // A managed symlink is trusted only while its generation marker and
    // binary name/hash agree. Mutation fails closed and keeps the link.
    let (tamper_bin, tamper_data) = (work.root.join("tamper-bin"), work.root.join("tamper-share/hamn/src"));
    work.install(&work.hamn, &tamper_bin, &tamper_data);
    let target = fs::read_link(tamper_bin.join("hamn")).unwrap();
    let mut bytes = fs::read(&target).unwrap();
    bytes.extend_from_slice(b"tampered\n");
    fs::write(&target, bytes).unwrap();
    work.refused(&work.hamn, &tamper_bin, &tamper_data, "a mutated managed generation");
    assert_eq!(fs::read_link(tamper_bin.join("hamn")).unwrap(), target);
}

/// Managed roots stay private from group/world writers; otherwise another
/// local user could alter the executable behind a trusted link.
fn group_writable_roots_are_refused() {
    let work = Work::new();
    let (bindir, datadir) = (work.root.join("mode-bin"), work.root.join("mode-share/hamn/src"));
    work.install(&work.hamn, &bindir, &datadir);
    let target = fs::read_link(bindir.join("hamn")).unwrap();
    for (root, label) in [
        (datadir.clone(), "data root"),
        (datadir.join(".hamn-generations"), "generation root"),
        (bindir.clone(), "binary root"),
    ] {
        fs::set_permissions(&root, fs::Permissions::from_mode(0o775)).unwrap();
        work.refused(&work.hamn, &bindir, &datadir, &format!("a group-writable {label}"));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(fs::read_link(bindir.join("hamn")).unwrap(), target, "{label}");
    }
}

/// Holds an installer at the test barrier `point` and SIGKILLs it there.
fn kill_at(work: &Work, point: &str, source: &Path, bindir: &Path, datadir: &Path) {
    let barrier = Barrier::new(&work.root, point, &format!("kill-{point}"));
    let mut command = work.command(source, bindir, datadir);
    let child = Group::spawn(barrier.apply(&mut command));
    barrier.await_ready(Duration::from_secs(20), point);
    child.signal_group(libc::SIGKILL);
    let result = child.finish(Duration::from_secs(5));
    assert_eq!(result.returncode, -libc::SIGKILL, "{}", result.stderr());
}

fn generation_directories(datadir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(datadir.join(".hamn-generations"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| !path.file_name().unwrap().to_string_lossy().starts_with('.'))
        .collect();
    entries.sort();
    entries
}

/// A kill before the first link commit leaves no command, but the data
/// marker and a complete immutable generation make a retry safe; the retry
/// collects that unreferenced generation.
fn kill_before_first_publication_leaves_a_collectible_generation() {
    let work = Work::new();
    let (bindir, datadir) = (work.root.join("fresh-kill-bin"), work.root.join("fresh-kill-share/hamn/src"));
    kill_at(&work, "BEFORE_LINK_PUBLICATION", &work.hamn, &bindir, &datadir);
    assert!(fs::symlink_metadata(bindir.join("hamn")).is_err());
    assert_eq!(fs::read_to_string(datadir.join(".hamn-managed")).unwrap(), "version=1\n");
    let orphans = generation_directories(&datadir);
    assert_eq!(orphans.len(), 1, "{orphans:?}");
    assert!(orphans[0].join(".hamn-generation").is_file(), "a pre-commit kill left an incomplete generation");
    assert_eq!(fs::read(orphans[0].join("bin/hamn")).unwrap(), fs::read(&work.hamn).unwrap());
    work.install(&work.hamn, &bindir, &datadir);
    assert_managed_install(&bindir, &datadir, &work.hamn);
    assert!(!orphans[0].exists(), "the unpublished generation was never collected");
}

/// SIGKILL immediately before the link rename leaves the old generation
/// active; immediately after it, a complete new generation.
fn kill_before_or_after_the_link_rename_keeps_one_complete_target() {
    let work = Work::new();
    let (bindir, datadir) = (work.root.join("kill-bin"), work.root.join("kill-share/hamn/src"));
    work.install(&work.hamn, &bindir, &datadir);
    let before_target = fs::read_link(bindir.join("hamn")).unwrap();
    let one = work.replacement("replacement-one");
    kill_at(&work, "BEFORE_LINK_PUBLICATION", &one, &bindir, &datadir);
    assert_eq!(fs::read_link(bindir.join("hamn")).unwrap(), before_target);
    assert!(before_target.is_file());
    assert_managed_install(&bindir, &datadir, &work.hamn);
    work.install(&one, &bindir, &datadir);
    assert_managed_install(&bindir, &datadir, &one);
    assert!(before_target.is_file(), "the predecessor was removed");

    let two = work.replacement("replacement-two");
    kill_at(&work, "AFTER_LINK_PUBLICATION", &two, &bindir, &datadir);
    assert_managed_install(&bindir, &datadir, &two);
    work.install(&two, &bindir, &datadir);
    assert_managed_install(&bindir, &datadir, &two);
}

/// Concurrent installers serialize before the first copy into a staged
/// generation: the second never reaches that point while the first holds it.
fn concurrent_installers_serialize_before_staging() {
    let work = Work::new();
    let (bindir, datadir) = (work.root.join("concurrent-bin"), work.root.join("concurrent-share/hamn/src"));
    let first_barrier = Barrier::new(&work.root, "STAGING", "first");
    let second_barrier = Barrier::new(&work.root, "STAGING", "second");
    let mut first_command = work.command(&work.hamn, &bindir, &datadir);
    let mut first = Group::spawn(first_barrier.apply(&mut first_command));
    first_barrier.await_ready(Duration::from_secs(20), "the first installer transaction entry");
    let mut second_command = work.command(&work.hamn, &bindir, &datadir);
    let mut second = Group::spawn(second_barrier.apply(&mut second_command));
    second_barrier.assert_no_ready(Duration::from_secs(1), "a concurrent installer transaction entry");
    assert!(first.running() && second.running());
    first_barrier.release(RUN);
    let result = first.finish(RUN);
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    second_barrier.await_ready(Duration::from_secs(20), "the waiting installer transaction entry");
    second_barrier.release(RUN);
    let result = second.finish(RUN);
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    assert_managed_install(&bindir, &datadir, &work.hamn);
}

fn overlapping_roots_are_refused() {
    let work = Work::new();
    let overlap = work.root.join("overlap");
    let stderr = work.refused(&work.hamn, &overlap, &overlap, "overlapping targets");
    assert!(stderr.contains("overlapping binary and data directories"), "{stderr}");
    assert!(fs::symlink_metadata(overlap.join(".hamn-generations")).is_err());
    let nested = work.root.join("nested");
    let stderr = work.refused(&work.hamn, &nested.join("bin"), &nested, "a binary root inside the data root");
    assert!(stderr.contains("overlapping binary and data directories"), "{stderr}");
}

/// `make install` (with the executable already built) publishes build/hamn
/// through the native installer into PREFIX.
fn make_install_publishes_the_built_executable() {
    let work = Work::new();
    let checkout = upgrade::checkout();
    let prefix = work.root.join("prefix");
    // Every install root is explicit, so no inherited BINDIR or DATADIR
    // can direct the recipe at a real installation.
    let result = upgrade::run(
        Command::new("make")
            .current_dir(&checkout)
            .args(["-o", "host", "install"])
            .arg(format!("PREFIX={}", prefix.display()))
            .arg(format!("BINDIR={}", prefix.join("bin").display()))
            .arg(format!("DATADIR={}", prefix.join("share/hamn/src").display()))
            .env("HOME", &work.home)
            .stdin(Stdio::null()),
        Duration::from_secs(120),
    );
    assert_eq!(result.returncode, 0, "{} {}", result.stdout(), result.stderr());
    let built = checkout.join("build/hamn");
    assert_managed_install(&prefix.join("bin"), &prefix.join("share/hamn/src"), &built);
    let version = upgrade::run(Command::new(prefix.join("bin/hamn")).arg("--version"), RUN);
    let expected = upgrade::run(Command::new(&built).arg("--version"), RUN);
    assert_eq!(version.stdout(), expected.stdout());
}
