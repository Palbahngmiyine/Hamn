//! Hamn 0.1.x installations migrate to this layout through the release
//! bootstrap (`update --bootstrap`, what install.sh runs) and the native
//! installer (`install`, what `make install` runs). Every case starts from a
//! genuine 0.1.2 install made by the 0.1.2 installer from Git history (see
//! `support::released`) in an owned HOME; nothing touches a real install,
//! the network or a VM.
//!
//! Contract: a migration publishes a new-layout generation with one atomic
//! link rename and keeps the 0.1.2 generation as its predecessor; a failure
//! at any step or a SIGKILL at any point leaves a command that works, the
//! 0.1.2 one until the transaction commits; a 0.1.x install failing the
//! checks it was installed with is refused unchanged.
use crate::runner::{self, case};
use crate::support::released;
use crate::support::upgrade::{self, Barrier, Group, Output, Releases, file_digest};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "released-migration",
        "Hamn 0.1.2 installs migrate atomically and roll back to a working 0.1.2 command",
        vec![
            case(
                "released_installer_leaves_the_layout_this_hamn_verifies",
                released_installer_leaves_the_layout_this_hamn_verifies,
            ),
            case(
                "bootstrap_migrates_a_released_install_and_later_collects_it",
                bootstrap_migrates_a_released_install_and_later_collects_it,
            ),
            case(
                "make_install_migrates_a_released_install_and_later_collects_it",
                make_install_migrates_a_released_install_and_later_collects_it,
            ),
            case(
                "killed_bootstrap_migration_always_leaves_a_working_command",
                killed_bootstrap_migration_always_leaves_a_working_command,
            ),
            case(
                "killed_make_install_migration_always_leaves_a_working_command",
                killed_make_install_migration_always_leaves_a_working_command,
            ),
            case(
                "failure_at_each_bootstrap_step_restores_the_released_install",
                failure_at_each_bootstrap_step_restores_the_released_install,
            ),
            case(
                "failure_at_each_make_install_step_keeps_one_working_command",
                failure_at_each_make_install_step_keeps_one_working_command,
            ),
            case(
                "released_install_failing_its_checks_is_refused_unchanged",
                released_install_failing_its_checks_is_refused_unchanged,
            ),
        ],
        filters,
    )
}

const RUN: Duration = Duration::from_secs(60);
const NEW: &str = "0.2.0";
const SOURCE_BUILD: &str = "0.3.0";

/// A genuine 0.1.2 install in an owned HOME plus a newer release.
struct Migration {
    releases: Releases,
    payload: PathBuf,
    old_target: PathBuf,
    old_tree: Tree,
    release: PathBuf,
    manifest: PathBuf,
    /// A source build as `make install` publishes it (`--version` reports
    /// `SOURCE_BUILD`; private operations run the Hamn under test).
    source_build: PathBuf,
}

/// Every entry below a root: type, mode, link count and content digest or
/// link target, except the permanent lock files and the recovery references
/// (`.hamn-recovery-root-*`) an update transaction records in the generation
/// it may roll back to.
type Tree = BTreeMap<PathBuf, String>;

fn tree(root: &Path) -> Tree {
    let mut entries = Tree::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(children) = fs::read_dir(&directory) else { continue };
        for child in children {
            let path = child.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with(".hamn-install.lock")
                || name.ends_with(".hamn-transaction.lock")
                || name.starts_with(".hamn-recovery-root-")
            {
                continue;
            }
            let m = fs::symlink_metadata(&path).unwrap();
            let content = if m.file_type().is_symlink() {
                format!("link {}", fs::read_link(&path).unwrap().display())
            } else if m.is_dir() {
                pending.push(path.clone());
                "directory".to_owned()
            } else {
                file_digest(&path)
            };
            entries.insert(path, format!("{:o}:{}:{content}", m.mode() & 0o7777, m.nlink()));
        }
    }
    entries
}

impl Migration {
    fn new(prefix: &str) -> Self {
        let releases = Releases::new(prefix);
        let payload = released::payload(&releases.root);
        let (old_target, output) = released::install(&payload, &releases.bindir, &releases.datadir, &releases.home);
        assert!(
            output.stdout().contains(&format!(
                "installed: {}/hamn -> {}",
                releases.bindir.display(),
                old_target.display()
            )),
            "{}",
            output.stdout()
        );
        let (release, manifest) = releases.release(NEW);
        let source_build = releases.root.join("source-build/hamn");
        fs::create_dir_all(source_build.parent().unwrap()).unwrap();
        let native = releases.root.join("native-hamn");
        upgrade::write_executable(&source_build, &upgrade::version_wrapper(SOURCE_BUILD, &native));
        let old_tree = tree(&upgrade::generation_of(&old_target));
        Self { releases, payload, old_target, old_tree, release, manifest, source_build }
    }

    fn generations(&self) -> PathBuf {
        self.releases.datadir.join(".hamn-generations")
    }

    /// `hamn --version` through the command link.
    fn version(&self) -> String {
        upgrade::run(Command::new(&self.releases.command).arg("--version").env("HOME", &self.releases.home), RUN)
            .stdout()
    }

    /// The 0.1.2 command is active, unchanged and working.
    fn assert_released_active(&self, what: &str) {
        assert_eq!(self.releases.active(), self.old_target, "{what}: the command no longer names the 0.1.2 generation");
        assert_eq!(
            tree(&upgrade::generation_of(&self.old_target)),
            self.old_tree,
            "{what}: the 0.1.2 generation changed"
        );
        assert_eq!(self.version(), format!("hamn {}\n", released::VERSION), "{what}: the 0.1.2 command does not work");
    }

    /// The command names a complete generation of this layout for `binary`
    /// whose predecessor is the 0.1.2 generation, which is kept unchanged.
    fn assert_migrated(&self, binary: &Path, version: &str, what: &str) -> PathBuf {
        let target = self.assert_new_active(binary, version, what);
        let generation = upgrade::generation_of(&target);
        assert_eq!(
            fs::read_to_string(generation.join(".hamn-previous-target")).unwrap(),
            format!("{}\n", self.old_target.display()),
            "{what}: the 0.1.2 generation is not the recorded predecessor"
        );
        assert_eq!(
            tree(&upgrade::generation_of(&self.old_target)),
            self.old_tree,
            "{what}: the 0.1.2 predecessor changed"
        );
        target
    }

    /// The command names a complete, working generation of this layout for
    /// `binary` (after a migration, possibly with a newer predecessor).
    fn assert_new_active(&self, binary: &Path, version: &str, what: &str) -> PathBuf {
        let target = self.releases.active();
        let generation = upgrade::generation_of(&target);
        assert_eq!(generation.parent(), Some(self.generations().as_path()), "{what}: {}", target.display());
        let hash = file_digest(binary);
        assert!(generation.file_name().unwrap().to_str().unwrap().starts_with(&format!("{hash}-")), "{what}");
        assert_eq!(fs::read(&target).unwrap(), fs::read(binary).unwrap(), "{what}");
        let path_id = |path: &Path| upgrade::digest(format!("{}\0", path.display()).as_bytes());
        assert_eq!(
            fs::read_to_string(generation.join(".hamn-generation")).unwrap(),
            format!(
                "version=2\nbinary_sha256={hash}\nbindir_id={}\ndatadir_id={}\n",
                path_id(&self.releases.bindir),
                path_id(&self.releases.datadir)
            ),
            "{what}"
        );
        assert_eq!(fs::metadata(generation.join(".hamn-generation")).unwrap().mode() & 0o7777, 0o600);
        assert!(fs::symlink_metadata(generation.join("share/hamn/src")).is_err(), "{what}: sources were installed");
        assert_eq!(self.version(), format!("hamn {version}\n"), "{what}");
        target
    }

    fn bootstrap(&self) -> Command {
        self.releases.updater(&self.release, &self.manifest, &["--bootstrap"])
    }

    fn make_install(&self) -> Command {
        let native = self.releases.root.join("native-hamn");
        upgrade::install_command(
            &native,
            &self.source_build,
            &self.releases.bindir,
            &self.releases.datadir,
            &self.releases.home,
        )
    }

    /// The command resolves to a complete generation and runs: the 0.1.2
    /// one, or the new one of `binary` reporting `version`.
    fn assert_one_working_command(&self, binary: &Path, version: &str, what: &str) {
        if self.releases.active() == self.old_target {
            self.assert_released_active(what);
        } else {
            self.assert_migrated(binary, version, what);
        }
    }
}

fn run(command: &mut Command) -> Output {
    upgrade::run(command, RUN)
}

/// The 0.1.2 installer's output is the layout this Hamn's released-layout
/// validation encodes; a divergence would make these cases test a fixture
/// rather than the release.
fn released_installer_leaves_the_layout_this_hamn_verifies() {
    let migration = Migration::new("hamn-released-layout-");
    let releases = &migration.releases;
    let generation = upgrade::generation_of(&migration.old_target);
    let binary = migration.payload.join("bin/hamn");
    let mode = |path: &Path| fs::symlink_metadata(path).unwrap().mode() & 0o7777;
    assert_eq!(mode(&generation), 0o755);
    assert_eq!(mode(&migration.old_target), 0o755);
    assert_eq!(fs::symlink_metadata(&migration.old_target).unwrap().nlink(), 1);
    assert_eq!(fs::read(&migration.old_target).unwrap(), fs::read(&binary).unwrap());
    let hash = file_digest(&binary);
    assert!(generation.file_name().unwrap().to_str().unwrap().starts_with(&format!("{hash}-")));
    let path_id = |path: &Path| upgrade::digest(format!("{}\0", path.display()).as_bytes());
    let marker = generation.join(".hamn-generation");
    assert_eq!(mode(&marker), 0o600);
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        format!(
            "version=1\nbinary_sha256={hash}\nbindir_id={}\ndatadir_id={}\n",
            path_id(&releases.bindir),
            path_id(&releases.datadir)
        )
    );
    assert_eq!(fs::read_to_string(generation.join(".hamn-retention")).unwrap(), "version=1\n");
    let source = generation.join("share/hamn/src");
    for directory in ["scripts", "packaging"] {
        assert!(fs::symlink_metadata(source.join(directory)).unwrap().is_dir(), "{directory}");
    }
    assert_eq!(mode(&source.join("scripts/update-host.sh")), 0o755);
    assert_eq!(
        fs::read(source.join("scripts/install-host.sh")).unwrap(),
        fs::read(migration.payload.join("scripts/install-host.sh")).unwrap()
    );
    let data_marker = releases.datadir.join(".hamn-managed");
    assert_eq!(fs::read_to_string(&data_marker).unwrap(), "version=1\n");
    assert_eq!(mode(&data_marker), 0o600);
    assert_eq!(mode(&releases.datadir), 0o755);
    migration.assert_released_active("the 0.1.2 installer");
}

fn bootstrap_migrates_a_released_install_and_later_collects_it() {
    let migration = Migration::new("hamn-released-bootstrap-");
    let result = run(&mut migration.bootstrap());
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    let stderr = result.stderr();
    assert!(stderr.lines().any(|line| line == format!("Updating Hamn {} → {NEW}...", released::VERSION)), "{stderr}");
    assert!(
        stderr.lines().any(|line| line == format!("Updated Hamn {} → {NEW}. Existing VMs were not restarted.", released::VERSION)),
        "{stderr}"
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!((value["completed"].as_bool(), value["currentVersion"].as_str()), (Some(true), Some(released::VERSION)));
    let target = migration.assert_migrated(&migration.release, NEW, "bootstrap migration");
    let generation = upgrade::generation_of(&target);
    let pointer = fs::read_to_string(generation.join("share/hamn/update-manifest-url")).unwrap();
    assert_eq!(pointer, "https://fixture.test/manifest-v3.json\n");
    // The receipt binds this release to the new generation's contents.
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(generation.join(".hamn-release.json")).unwrap()).expect("release receipt");
    assert_eq!(receipt["schemaVersion"], 2, "{receipt}");
    assert!(receipt.to_string().contains(NEW), "{receipt}");
    assert!(fs::symlink_metadata(migration.releases.journal()).is_err());
    assert!(migration.releases.selection.is_file(), "the guest image selection was not committed");
    // The next update supersedes the migrated generation; the 0.1.2
    // generation is then unreferenced and collected like any other.
    let (later, later_manifest) = migration.releases.release("0.4.0");
    let result = run(&mut migration.releases.updater(&later, &later_manifest, &["--bootstrap"]));
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    assert!(
        !upgrade::generation_of(&migration.old_target).exists(),
        "the superseded 0.1.2 generation was never collected"
    );
    assert!(target.is_file(), "the predecessor of the active generation was collected");
    assert_eq!(migration.version(), "hamn 0.4.0\n");
}

fn make_install_migrates_a_released_install_and_later_collects_it() {
    let migration = Migration::new("hamn-released-make-install-");
    let result = run(&mut migration.make_install());
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    let target = migration.assert_migrated(&migration.source_build, SOURCE_BUILD, "make install migration");
    assert!(
        result.stdout().contains(&format!(
            "installed: {}/hamn -> {}",
            migration.releases.bindir.display(),
            target.display()
        )),
        "{}",
        result.stdout()
    );
    assert!(
        fs::symlink_metadata(upgrade::generation_of(&target).join("share")).is_err(),
        "a source build has no pointer"
    );
    // A later make install collects the then unreferenced 0.1.2 generation.
    let mut bytes = fs::read(&migration.source_build).unwrap();
    bytes.extend_from_slice(b"# later build\n");
    fs::write(&migration.source_build, bytes).unwrap();
    let result = run(&mut migration.make_install());
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    let old = upgrade::generation_of(&migration.old_target);
    assert!(!old.exists(), "the superseded 0.1.2 generation was never collected");
    assert!(
        result
            .stdout()
            .contains(&format!("hamn: removed obsolete generation {}", old.file_name().unwrap().to_str().unwrap())),
        "{}",
        result.stdout()
    );
    assert!(target.is_file());
}

/// SIGKILLs a migration at `point`: the command must work then, and a
/// rerun completes the migration.
fn kill_at(migration: &Migration, mut command: Command, point: &str, binary: &Path, version: &str) {
    let barrier = Barrier::new(&migration.releases.root, point, &format!("kill-{point}"));
    let child = Group::spawn(barrier.apply(&mut command));
    barrier.await_ready(Duration::from_secs(30), point);
    child.signal_group(libc::SIGKILL);
    let killed = child.finish(Duration::from_secs(10));
    assert_eq!(killed.returncode, -libc::SIGKILL, "{point}: {}", killed.stderr());
    migration.assert_one_working_command(binary, version, &format!("SIGKILL at {point}"));
}

fn killed_bootstrap_migration_always_leaves_a_working_command() {
    for point in [
        "BEFORE_LINK_PUBLICATION",
        "LINK_STAGED",
        "AFTER_LINK_PUBLICATION",
        "AFTER_HOST_INSTALL",
        "AFTER_GUEST_SELECTION",
    ] {
        let migration = Migration::new("hamn-released-bootstrap-kill-");
        kill_at(&migration, migration.bootstrap(), point, &migration.release, NEW);
        // Before the link rename the 0.1.2 command is untouched; after it
        // the new, complete generation runs until recovery restores 0.1.2.
        if matches!(point, "BEFORE_LINK_PUBLICATION" | "LINK_STAGED") {
            migration.assert_released_active(&format!("SIGKILL at {point}"));
        } else {
            migration.assert_migrated(&migration.release, NEW, &format!("SIGKILL at {point}"));
        }
        assert!(migration.releases.journal().exists(), "{point}: the killed migration left no recovery journal");
        // Recovery restores the 0.1.2 command before the rerun migrates.
        let result = run(&mut migration.bootstrap());
        assert_eq!(result.returncode, 0, "{point}: {}", result.stderr());
        assert!(
            result.stderr().contains("recovered the previous binary and guest image selection"),
            "{point}: {}",
            result.stderr()
        );
        migration.assert_migrated(&migration.release, NEW, &format!("rerun after SIGKILL at {point}"));
        assert!(fs::symlink_metadata(migration.releases.journal()).is_err(), "{point}");
    }
}

fn killed_make_install_migration_always_leaves_a_working_command() {
    for point in ["STAGING", "BEFORE_LINK_PUBLICATION", "LINK_STAGED", "AFTER_LINK_PUBLICATION"] {
        let migration = Migration::new("hamn-released-make-install-kill-");
        kill_at(&migration, migration.make_install(), point, &migration.source_build, SOURCE_BUILD);
        if point == "AFTER_LINK_PUBLICATION" {
            migration.assert_migrated(&migration.source_build, SOURCE_BUILD, "SIGKILL after the link rename");
        } else {
            migration.assert_released_active(&format!("SIGKILL at {point}"));
        }
        let result = run(&mut migration.make_install());
        assert_eq!(result.returncode, 0, "{point}: {}", result.stderr());
        let what = format!("rerun after SIGKILL at {point}");
        if point == "AFTER_LINK_PUBLICATION" {
            // The killed run had committed; the rerun replaces its generation.
            migration.assert_new_active(&migration.source_build, SOURCE_BUILD, &what);
        } else {
            migration.assert_migrated(&migration.source_build, SOURCE_BUILD, &what);
        }
    }
}

fn with_fault(mut command: Command, fault: &str) -> Output {
    run(command.env("HAMN_TEST_UPDATE_FAULTS", fault))
}

fn failure_at_each_bootstrap_step_restores_the_released_install() {
    let migration = Migration::new("hamn-released-bootstrap-fault-");
    // Each journaled step, in transaction order, with how it reports the
    // rollback. Every one leaves the 0.1.2 command active and working and
    // no guest image selection (there was none before).
    for (fault, reported) in [
        ("PREPARED", "prepared transaction barrier failed; prior binary and guest image selection were restored"),
        ("host-install", "host install failed; prior binary and guest image selection were restored"),
        ("STAGING", "host install failed; prior binary and guest image selection were restored"),
        ("BEFORE_LINK_PUBLICATION", "host install failed; prior binary and guest image selection were restored"),
        ("LINK_STAGED", "host install failed; prior binary and guest image selection were restored"),
        ("AFTER_LINK_PUBLICATION", "host install failed; prior binary and guest image selection were restored"),
        (
            "AFTER_HOST_INSTALL",
            "update interruption barrier failed; prior binary and guest image selection were restored",
        ),
        ("receipt-write", "release receipt failed; update transaction recovered"),
        ("AFTER_GUEST_SELECTION", "guest selection barrier failed"),
    ] {
        let result = with_fault(migration.bootstrap(), fault);
        assert_eq!(result.returncode, 1, "{fault}: {}", result.stderr());
        assert!(result.stdout.is_empty(), "{fault}: a failed migration reported a result");
        assert!(result.stderr().contains(&format!("hamn install: {reported}")), "{fault}: {}", result.stderr());
        migration.assert_released_active(&format!("failure at {fault}"));
        assert!(
            fs::symlink_metadata(migration.releases.journal()).is_err(),
            "{fault}: the rollback left a pending journal"
        );
        assert!(!migration.releases.selection.exists(), "{fault}: the rollback kept a guest image selection");
    }
    // When the journal itself cannot be retired, the rollback restores the
    // 0.1.2 command but its own retirement fails too: the journal stays
    // pending (blocking VM start) until the next run recovers it.
    let result = with_fault(migration.bootstrap(), "retire-journal");
    assert_eq!(result.returncode, 1, "{}", result.stderr());
    assert!(result.stderr().contains("update commit could not clear its recovery journal"), "{}", result.stderr());
    migration.assert_released_active("failure to retire the journal");
    assert!(migration.releases.journal().exists(), "the unretired journal disappeared");
    // After the commit point the migration stands even if a later step
    // fails; this run first recovers the pending journal.
    let result = with_fault(migration.bootstrap(), "AFTER_JOURNAL_RETIRE");
    assert!(result.stderr().contains("recovered the previous binary and guest image selection"), "{}", result.stderr());
    assert_eq!(result.returncode, 1, "{}", result.stderr());
    assert!(
        result.stderr().contains("update completion barrier failed; the completed transaction is safely retired"),
        "{}",
        result.stderr()
    );
    migration.assert_migrated(&migration.release, NEW, "failure after the commit");
    // Complete but unpublished generations of the failed attempts are
    // collected by the next successful transaction.
    let result = run(&mut migration.bootstrap());
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    let remaining: Vec<PathBuf> =
        fs::read_dir(migration.generations()).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(remaining.len(), 2, "only the active generation and its 0.1.2 predecessor remain: {remaining:?}");
}

fn failure_at_each_make_install_step_keeps_one_working_command() {
    let migration = Migration::new("hamn-released-make-install-fault-");
    for fault in ["host-install", "STAGING", "BEFORE_LINK_PUBLICATION", "LINK_STAGED"] {
        let result = with_fault(migration.make_install(), fault);
        assert_ne!(result.returncode, 0, "{fault}");
        assert!(result.stderr().contains(&format!("injected {fault} fault")), "{fault}: {}", result.stderr());
        migration.assert_released_active(&format!("failure at {fault}"));
    }
    // The link rename is the commit: a failure after it reports the error
    // with the new generation complete and active.
    let result = with_fault(migration.make_install(), "AFTER_LINK_PUBLICATION");
    assert_ne!(result.returncode, 0);
    migration.assert_migrated(&migration.source_build, SOURCE_BUILD, "failure after the link rename");
    let result = run(&mut migration.make_install());
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    migration.assert_new_active(&migration.source_build, SOURCE_BUILD, "retry after the committed failure");
}

fn released_install_failing_its_checks_is_refused_unchanged() {
    let migration = Migration::new("hamn-released-refusal-");
    let releases = &migration.releases;
    let generation = upgrade::generation_of(&migration.old_target);
    let scripts = generation.join("share/hamn/src/scripts");
    let marker = generation.join(".hamn-generation");
    let marker_text = fs::read_to_string(&marker).unwrap();
    let binary = fs::read(&migration.old_target).unwrap();
    let alias = releases.root.join("hamn-alias");
    let aside = releases.root.join("scripts-aside");
    let data_marker = releases.datadir.join(".hamn-managed");
    let refusal = format!(
        "{}/hamn points to a Hamn 0.1.x generation that fails the ownership checks it was installed with, \
         so this Hamn cannot migrate it; move {}/hamn and {} aside, then reinstall with install.sh",
        releases.bindir.display(),
        releases.bindir.display(),
        releases.datadir.display()
    );
    let empty_marker = format!("{} is a pre-release Hamn install (empty data marker)", releases.datadir.display());
    let set_mode = |path: &Path, mode: u32| fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    let tamperings: Vec<(&str, &str, Box<dyn Fn(bool) + '_>)> = vec![
        (
            "a changed binary",
            refusal.as_str(),
            Box::new(|on| {
                let mut bytes = binary.clone();
                if on {
                    bytes.extend_from_slice(b"#\n");
                }
                fs::write(&migration.old_target, bytes).unwrap();
            }),
        ),
        (
            "a second link to the binary",
            refusal.as_str(),
            Box::new(|on| {
                if on {
                    fs::hard_link(&migration.old_target, &alias).unwrap();
                } else {
                    fs::remove_file(&alias).unwrap();
                }
            }),
        ),
        (
            "a group-writable generation",
            refusal.as_str(),
            Box::new(|on| set_mode(&generation, if on { 0o775 } else { 0o755 })),
        ),
        (
            "a group-writable binary",
            refusal.as_str(),
            Box::new(|on| set_mode(&migration.old_target, if on { 0o775 } else { 0o755 })),
        ),
        (
            "scripts behind a symbolic link",
            refusal.as_str(),
            Box::new(|on| {
                if on {
                    fs::rename(&scripts, &aside).unwrap();
                    std::os::unix::fs::symlink(&aside, &scripts).unwrap();
                } else {
                    fs::remove_file(&scripts).unwrap();
                    fs::rename(&aside, &scripts).unwrap();
                }
            }),
        ),
        (
            "an extra marker line",
            refusal.as_str(),
            Box::new(|on| {
                let text = if on { format!("{marker_text}extra=1\n") } else { marker_text.clone() };
                fs::write(&marker, text).unwrap();
            }),
        ),
        (
            "a marker for other roots",
            refusal.as_str(),
            Box::new(|on| {
                let text = if on { marker_text.replacen("bindir_id=", "bindir_id=0", 1) } else { marker_text.clone() };
                fs::write(&marker, text).unwrap();
            }),
        ),
        ("a readable marker", refusal.as_str(), Box::new(|on| set_mode(&marker, if on { 0o644 } else { 0o600 }))),
        (
            "an empty data marker",
            empty_marker.as_str(),
            Box::new(|on| {
                fs::write(&data_marker, if on { "" } else { "version=1\n" }).unwrap();
            }),
        ),
    ];
    for (what, message, tamper) in &tamperings {
        tamper(true);
        let before = tree(&releases.home);
        for (path, command) in [("make install", migration.make_install()), ("bootstrap", migration.bootstrap())] {
            let mut command = command;
            let result = run(&mut command);
            assert_ne!(result.returncode, 0, "{path} migrated {what}");
            assert!(result.stderr().contains(*message), "{path}, {what}: {}", result.stderr());
            assert_eq!(tree(&releases.home), before, "{path} changed an install with {what}");
        }
        tamper(false);
    }
    migration.assert_released_active("after the refusals");
}
