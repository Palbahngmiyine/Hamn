//! `hamn upgrade` and `--headless system upgrade` on a real managed install:
//! read-only checks, one-line failures, the zero-payload no-op, guest-only
//! repair, `--force`, downgrade and direct-generation refusal, and the
//! interruption and cancellation of selection-only and host transactions.
//! Everything runs in an owned HOME with local release artifacts; no VM.
use crate::runner::{self, case};
use crate::support::tmp::TempDir;
use crate::support::upgrade::{
    self, Artifact, Group, Output, await_ready, file_digest, mkfifo, pack_release, ready_fifo, release_payload, write_json,
};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "upgrade-cli",
        "upgrade command, immutable check, zero-payload no-op/force, guest-only repair, downgrade and direct-generation refusal",
        vec![case("upgrade_command_checks_repairs_and_recovers", upgrade_command_checks_repairs_and_recovers)],
        filters,
    )
}

const RUN: Duration = Duration::from_secs(60);

struct Install {
    root: PathBuf,
    home: PathBuf,
    bindir: PathBuf,
    datadir: PathBuf,
    command: PathBuf,
    manifest: PathBuf,
    _directory: TempDir,
}

impl Install {
    fn env<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command.env("HOME", &self.home).env("TMPDIR", &self.root).env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1")
    }

    /// `hamn ARGS... --manifest MANIFEST` through the managed command link.
    fn run(&self, args: &[&str], success: bool) -> Output {
        let mut command = Command::new(&self.command);
        command.args(args).arg("--manifest").arg(&self.manifest);
        let result = upgrade::run(self.env(&mut command), RUN);
        assert_eq!(result.returncode == 0, success, "{args:?}: {} {}", result.stdout(), result.stderr());
        result
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.run(args, true).stdout).expect("JSON result")
    }

    fn active(&self) -> PathBuf {
        fs::read_link(&self.command).unwrap()
    }

    fn journal(&self) -> PathBuf {
        self.home.join(".hamn/cache/.hamn-update-transaction")
    }
}

fn upgrade_command_checks_repairs_and_recovers() {
    let hamn = crate::support::hamn();
    let version = upgrade::run(Command::new(&hamn).arg("--version"), Duration::from_secs(10)).stdout();
    let version = version.trim().strip_prefix("hamn ").expect("hamn VERSION").to_owned();
    // An unmanaged (source) build reports unsupported-install offline.
    let source_check = upgrade::run(
        Command::new(&hamn).args(["upgrade", "--check", "--manifest", "https://unreachable.invalid/manifest", "--output", "json"]),
        Duration::from_secs(10),
    );
    assert_eq!(source_check.returncode, 0, "{}", source_check.stderr());
    let value: Value = serde_json::from_slice(&source_check.stdout).unwrap();
    assert_eq!((value["status"].clone(), value["downloadedBytes"].clone()), ("unsupported-install".into(), 0.into()));

    let directory = TempDir::new("hamn-upgrade-cli-");
    let root = fs::canonicalize(directory.path()).unwrap();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let (bindir, datadir) = (home.join("bin"), home.join("source"));
    upgrade::install(&hamn, &hamn, &bindir, &datadir, &home);
    let release = root.join("release");
    release_payload(&release, &hamn, "https://example.invalid/manifest-v3.json");
    let archive = root.join("host.tar.gz");
    pack_release(&release, &archive);
    let guest = root.join("guest.img");
    fs::write(&guest, "guest-only repair fixture\n").unwrap();
    let mut manifest = upgrade::manifest(&format!("v{version}"), &Artifact::local(&archive), &Artifact::local(&guest));
    manifest["commit"] = "1".repeat(40).into();
    let install = Install {
        command: bindir.join("hamn"),
        manifest: root.join("manifest.json"),
        root,
        home,
        bindir,
        datadir,
        _directory: directory,
    };
    write_json(&install.manifest, &manifest);
    let original = install.active();

    checks_and_failures_change_nothing(&install, &original);
    let installed = install.json(&["upgrade", "--output", "json"]);
    assert_eq!((installed["status"].clone(), installed["profileDisksChanged"].clone()), ("updated".into(), false.into()));
    let active = install.active();
    assert_ne!(active, original);
    let profile = install.home.join(".hamn/owned-profile");
    fs::create_dir(&profile).unwrap();
    let disk = profile.join("disk.img");
    fs::write(&disk, "must remain unchanged").unwrap();
    let disk_before = file_digest(&disk);
    let repeated = install.json(&["upgrade", "--output", "json"]);
    assert_eq!((repeated["status"].clone(), repeated["downloadedBytes"].clone()), ("up-to-date".into(), 0.into()));
    let archive_size = fs::metadata(&archive).unwrap().len();
    let guest_size = fs::metadata(&guest).unwrap().len();
    assert_eq!(repeated["reusedBytes"], archive_size + guest_size);
    assert_eq!(install.active(), active);

    let selection = install.home.join(".hamn/cache/guest-image.json");
    let selected: Value = serde_json::from_slice(&fs::read(&selection).unwrap()).unwrap();
    let cached = selection.parent().unwrap().join(selected["file"].as_str().unwrap());
    // A matching digest/receipt cannot override a contradictory v3 byte size.
    // Check and mutation must agree, and failure must not publish a journal
    // or alter the active generation, selected image, or profile data.
    let selected_before = fs::read(&selection).unwrap();
    for declared in [guest_size - 1, guest_size + 1] {
        manifest["artifacts"]["guestImage"]["size"] = declared.into();
        write_json(&install.manifest, &manifest);
        assert_eq!(install.json(&["upgrade", "--check", "--output", "json"])["status"], "repair-required");
        let mut receipt = Command::new(&install.command);
        receipt
            .args(["__install-support", "upgrade", "receipt"])
            .arg(&install.manifest)
            .arg("check")
            .arg(&active)
            .arg(selection.parent().unwrap());
        let receipt = upgrade::run(install.env(&mut receipt), Duration::from_secs(10));
        assert_ne!(receipt.returncode, 0, "receipt accepted a contradictory guest byte size");
        install.run(&["upgrade", "--output", "json"], false);
        assert_eq!(install.active(), active);
        assert_eq!(fs::read(&selection).unwrap(), selected_before);
        assert_eq!(file_digest(&disk), disk_before);
        assert_eq!(fs::read(&cached).unwrap(), fs::read(&guest).unwrap());
        assert!(!install.journal().exists());
    }
    manifest["artifacts"]["guestImage"]["size"] = guest_size.into();
    write_json(&install.manifest, &manifest);
    assert_eq!(install.json(&["upgrade", "--output", "json"])["status"], "up-to-date");
    fs::write(&cached, "damaged guest image").unwrap();
    let repaired = install.json(&["upgrade", "--output", "json"]);
    assert_eq!((repaired["status"].clone(), repaired["artifacts"]["host"]["downloadedBytes"].clone()), ("repaired".into(), 0.into()));
    assert_eq!(install.active(), active);
    assert_eq!(fs::read(&cached).unwrap(), fs::read(&guest).unwrap());
    fs::remove_file(&selection).unwrap();
    assert_eq!(install.json(&["upgrade", "--output", "json"])["status"], "repaired");
    assert_eq!(install.active(), active);
    let forced = install.json(&["upgrade", "--force", "--output", "json"]);
    assert_eq!((forced["status"].clone(), forced["downloadedBytes"].clone()), ("updated".into(), 0.into()));
    assert_ne!(install.active(), active);
    let active = install.active();
    let headless = install.json(&["--headless", "system", "upgrade", "--check"]);
    assert_eq!((headless["ok"].clone(), headless["data"]["status"].clone()), (true.into(), "up-to-date".into()));
    install.run(&["upgrade", "--check", "--force"], false);
    let saved = fs::read(&install.manifest).unwrap();
    manifest["version"] = "v0.0.0".into();
    write_json(&install.manifest, &manifest);
    assert_eq!(install.json(&["upgrade", "--check", "--output", "json"])["status"], "ahead");
    install.run(&["upgrade", "--output", "json"], false);
    assert_eq!(install.active(), active);
    assert_eq!(file_digest(&disk), disk_before);
    fs::write(&install.manifest, &saved).unwrap();

    // A generation binary run directly cannot upgrade itself; its check is
    // unsupported-install.
    let mut direct = Command::new(&active);
    direct.args(["upgrade", "--manifest"]).arg(&install.manifest);
    let direct = upgrade::run(install.env(&mut direct), Duration::from_secs(10));
    assert_ne!(direct.returncode, 0);
    assert!(direct.stderr().contains("managed hamn command symlink"), "{}", direct.stderr());
    let mut direct_check = Command::new(&active);
    direct_check.args(["upgrade", "--check", "--manifest", "https://unreachable.invalid/manifest", "--output", "json"]);
    let direct_check = upgrade::run(install.env(&mut direct_check), Duration::from_secs(10));
    assert_eq!(direct_check.returncode, 0, "{}", direct_check.stderr());
    assert_eq!(serde_json::from_slice::<Value>(&direct_check.stdout).unwrap()["status"], "unsupported-install");

    selection_only_interruptions_recover(&install, &active, &selection);
    frontend_cancellation_waits_for_rollback(&install, &active, &selection, &disk, &disk_before);

    // A pending journal is not touched by a manifest-only check.
    let journal = install.journal();
    fs::create_dir(&journal).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    let sentinel = journal.join("state");
    fs::write(&sentinel, "version=3\npartial fixture\n").unwrap();
    fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&sentinel).unwrap();
    install.run(&["upgrade", "--check", "--output", "json"], true);
    assert_eq!(fs::read(&sentinel).unwrap(), before);
    assert_eq!(install.active(), active);
    assert_eq!(file_digest(&disk), disk_before);
}

/// Read-only checks and unreachable-server failures, before the first
/// upgrade: nothing is created or changed.
fn checks_and_failures_change_nothing(install: &Install, original: &Path) {
    let checked = install.json(&["upgrade", "--check", "--output", "json"]);
    assert_eq!(checked["status"], "repair-required", "{checked}");
    assert_eq!(install.active(), original);
    assert!(!install.home.join(".hamn").exists());
    // An unreachable release server is one actionable line, for the
    // read-only check and for a mutation, and changes nothing.
    let unreachable = "https://127.0.0.1:9/manifest-v3.json";
    let expected = "could not check for updates: could not connect to the release server (curl exit 7). \
                    Check your connection and try again.";
    for (args, heading) in [(&["upgrade", "--check"][..], ""), (&["upgrade"][..], "Checking for updates...\n")] {
        let mut command = Command::new(&install.command);
        command.args(args).args(["--manifest", unreachable]);
        let failed = upgrade::run(install.env(&mut command), RUN);
        assert_eq!((failed.returncode, failed.stdout()), (1, String::new()), "{args:?}");
        assert_eq!(failed.stderr(), format!("{heading}hamn upgrade: {expected}\n"), "{args:?}");
    }
    let mut headless = Command::new(&install.command);
    headless.args(["--headless", "system", "upgrade", "--check", "--manifest", unreachable]);
    let failed = upgrade::run(install.env(&mut headless), RUN);
    let envelope: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(envelope["error"]["message"], expected, "{envelope}");
    assert_eq!(install.active(), original);
    assert!(!install.journal().exists());
}

/// Selection-only journal interruption matrix, including recovery followed by
/// a malformed manifest. No host pointer may be rewritten.
fn selection_only_interruptions_recover(install: &Install, active: &Path, selection: &Path) {
    // The installed generation's own executable runs the transaction.
    let helper = active.to_path_buf();
    for point in ["PREPARED", "AFTER_GUEST_SELECTION"] {
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGKILL] {
            let _ = fs::remove_file(selection);
            let (ready, ready_fd) = ready_fifo(&install.root, "ready");
            let release = install.root.join("release-fifo");
            mkfifo(&release);
            let mut command = Command::new(&helper);
            command
                .args(["__install-support", "update", "--bindir"])
                .arg(&install.bindir)
                .arg("--datadir")
                .arg(&install.datadir)
                .arg("--manifest")
                .arg(&install.manifest)
                .env(format!("HAMN_TEST_UPDATE_{point}_READY_FIFO"), &ready)
                .env(format!("HAMN_TEST_UPDATE_{point}_RELEASE_FIFO"), &release);
            let child = Group::spawn(install.env(&mut command));
            await_ready(&ready_fd, Duration::from_secs(30), point);
            let state = fs::read_to_string(install.journal().join("state")).unwrap();
            assert!(state.starts_with("version=3\n") && state.contains("hostMutation=0"), "{state}");
            child.signal(signal);
            let stopped = child.finish(Duration::from_secs(15));
            assert_ne!(stopped.returncode, 0, "{point} {signal}: {}", stopped.stderr());
            assert_eq!(install.active(), active);
            if signal == libc::SIGKILL {
                assert!(install.journal().is_dir());
                let saved = fs::read(&install.manifest).unwrap();
                fs::write(&install.manifest, "{").unwrap();
                install.run(&["upgrade", "--output", "json"], false);
                fs::write(&install.manifest, saved).unwrap();
            }
            assert!(!install.journal().exists() && !selection.exists(), "{point} {signal}: {}", stopped.stderr());
            install.run(&["upgrade", "--output", "json"], true);
            fs::remove_file(&ready).unwrap();
            fs::remove_file(&release).unwrap();
        }
    }
}

/// Frontend cancellation must wait for its owned worker/helper rollback, not
/// merely kill the Rust worker and leave its installer running.
fn frontend_cancellation_waits_for_rollback(install: &Install, active: &Path, selection: &Path, disk: &Path, disk_before: &str) {
    for operation in [&["upgrade", "--force", "--output", "json"][..], &["--headless", "system", "upgrade", "--yes", "--force"][..]] {
        let (ready, ready_fd) = ready_fifo(&install.root, "frontend-ready");
        let release = install.root.join("frontend-release");
        mkfifo(&release);
        let saved_selection = fs::read(selection).unwrap();
        let mut command = Command::new(&install.command);
        command
            .args(operation)
            .arg("--manifest")
            .arg(&install.manifest)
            .env("HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_READY_FIFO", &ready)
            .env("HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_RELEASE_FIFO", &release);
        let child = Group::spawn(install.env(&mut command));
        await_ready(&ready_fd, Duration::from_secs(30), "frontend barrier");
        child.signal(libc::SIGTERM);
        let stopped = child.finish(Duration::from_secs(20));
        assert_ne!(stopped.returncode, 0, "{operation:?}: {}", stopped.stderr());
        assert_eq!(install.active(), active, "{operation:?}: {}", stopped.stderr());
        assert_eq!(fs::read(selection).unwrap(), saved_selection);
        assert!(!install.journal().exists(), "{operation:?}: {}", stopped.stderr());
        assert_eq!(file_digest(disk), disk_before);
        fs::remove_file(&ready).unwrap();
        fs::remove_file(&release).unwrap();
    }
}
