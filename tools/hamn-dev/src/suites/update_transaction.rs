//! Immutable-release updates through the real `hamn --headless system
//! upgrade` frontend, core worker and native updater of real rebuilt
//! versions (0.0.1 → 0.0.2 → 0.0.3): installer failure, TERM and SIGKILL
//! at the host cutover, recovery before manifest validation, legacy
//! journals, and the no-op. Rebuilds build/hamn (`make host VERSION=...`)
//! and restores the version it found; run alone, from the checkout.
use crate::runner::{self, case};
use crate::support::exec::output_within;
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, Artifact, Barrier, Group, Output, file_digest};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "update-transaction",
        "immutable update rolls back installer failure and interruption safely",
        vec![case(
            "updates_recover_failure_interruption_and_legacy_journals",
            updates_recover_failure_interruption_and_legacy_journals,
        )],
        filters,
    )
}

const RUN: Duration = Duration::from_secs(120);
const BUILD: Duration = Duration::from_secs(3600);

/// `make host VERSION=version` in the checkout (replacing build/hamn).
fn build(version: &str) {
    let output =
        output_within(Command::new("make").args(["host", &format!("VERSION={version}")]).stdin(Stdio::null()), BUILD);
    assert!(output.status.success(), "make host VERSION={version}: {}", String::from_utf8_lossy(&output.stderr));
}

/// Rebuilds build/hamn at the version found before this suite replaced it.
struct RestoreHost(String);

impl Drop for RestoreHost {
    fn drop(&mut self) {
        let output = output_within(
            Command::new("make").args(["host", &format!("VERSION={}", self.0)]).stdin(Stdio::null()),
            BUILD,
        );
        if !output.status.success() {
            eprintln!("update-transaction: cannot restore build/hamn {}: {output:?}", self.0);
        }
    }
}

struct Fixture {
    work: PathBuf,
    home: PathBuf,
    bindir: PathBuf,
    datadir: PathBuf,
    _directory: TempDir,
}

impl Fixture {
    fn command(&self) -> PathBuf {
        self.bindir.join("hamn")
    }

    fn cache(&self) -> PathBuf {
        self.home.join(".hamn/cache")
    }

    fn selection_digest(&self) -> String {
        file_digest(&self.cache().join("guest-image.json"))
    }

    fn journal(&self) -> PathBuf {
        self.cache().join(".hamn-update-transaction")
    }

    fn active(&self) -> PathBuf {
        fs::read_link(self.command()).unwrap()
    }

    /// `hamn --headless system upgrade --yes --manifest MANIFEST` through
    /// the managed command link (the real frontend and worker).
    fn run_update(&self, manifest: &Path, faults: Option<&str>) -> Output {
        let mut command = Command::new(self.command());
        command
            .args(["--headless", "system", "upgrade", "--yes", "--manifest"])
            .arg(manifest)
            .env("HOME", &self.home)
            .env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1")
            .stdin(Stdio::null());
        if let Some(faults) = faults {
            command.env("HAMN_TEST_UPDATE_FAULTS", faults);
        }
        upgrade::run(&mut command, RUN)
    }

    fn assert_active_state(&self, target: &Path, selection: &str, label: &str) {
        assert_eq!(self.active(), target, "{label} changed the managed binary target");
        assert_eq!(self.selection_digest(), selection, "{label} changed the guest image selection");
        assert!(fs::symlink_metadata(self.journal()).is_err(), "{label} left an active update transaction");
    }

    /// A release built from build/hamn at `version`: its archive (a
    /// generation payload), a guest image with `guest_text`, and a v3
    /// manifest of local artifacts.
    fn release(&self, version: &str, guest_text: &str, tag: &str) -> PathBuf {
        let name = format!("hamn-v{version}{tag}-darwin-arm64");
        let root = self.work.join(&name);
        upgrade::release_payload(&root, Path::new("build/hamn"), "https://example.invalid/hamn-update-manifest.json");
        let archive = self.work.join(format!("host-v{version}{tag}.tar.gz"));
        let packed = Command::new("/usr/bin/tar")
            .env("COPYFILE_DISABLE", "1")
            .arg("-C")
            .arg(&self.work)
            .arg("-czf")
            .arg(&archive)
            .arg(&name)
            .output()
            .unwrap();
        assert!(packed.status.success(), "tar: {}", String::from_utf8_lossy(&packed.stderr));
        let guest = self.work.join(format!("guest-v{version}{tag}.img"));
        fs::write(&guest, format!("{guest_text}\n")).unwrap();
        let mut manifest =
            upgrade::manifest(&format!("v{version}"), &Artifact::local(&archive), &Artifact::local(&guest));
        manifest["commit"] = "0123456789abcdef0123456789abcdef01234567".into();
        let path = self.work.join(format!("manifest-v{version}{tag}.json"));
        upgrade::write_json(&path, &manifest);
        path
    }

    fn version(&self) -> String {
        upgrade::run(Command::new(self.command()).arg("--version").env("HOME", &self.home), RUN).stdout()
    }
}

fn updates_recover_failure_interruption_and_legacy_journals() {
    let original = upgrade::run(Command::new("build/hamn").arg("--version"), RUN).stdout();
    let original = original.split_whitespace().nth(1).expect("hamn VERSION").to_owned();
    let _restore = RestoreHost(original);
    let directory = TempDir::new("hamn-update-");
    let work = fs::canonicalize(directory.path()).unwrap();
    let fixture = Fixture {
        home: work.join("home"),
        bindir: work.join("bin"),
        datadir: work.join("share/hamn/src"),
        work,
        _directory: directory,
    };
    fs::create_dir(&fixture.home).unwrap();
    build("0.0.1");
    let old = fixture.work.join("old-hamn");
    fs::copy("build/hamn", &old).unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    upgrade::install(&old, &old, &fixture.bindir, &fixture.datadir, &fixture.home);
    let old_target = fixture.active();

    build("0.0.2");
    let manifest_2 = fixture.release("0.0.2", "immutable guest image v0.0.2", "");
    let result = fixture.run_update(&manifest_2, None);
    assert_eq!(result.returncode, 0, "{} {}", result.stdout(), result.stderr());
    assert!(
        result.stderr().lines().any(|line| line == "Updated Hamn 0.0.1 → 0.0.2. Existing VMs were not restarted."),
        "{}",
        result.stderr()
    );
    assert!(result.stdout().contains("\"completed\":true"), "{}", result.stdout());
    let new_target = fixture.active();
    assert_ne!(new_target, old_target, "the update did not switch the managed binary");
    assert_eq!(fixture.version(), "hamn 0.0.2\n");
    assert!(fs::read_to_string(fixture.cache().join("guest-image.json")).unwrap().contains("hamn-guest-"));
    let selection_2 = fixture.selection_digest();

    // A direct generation binary cannot update itself, and a modified
    // manifest cannot change either the generation or the guest image.
    let direct = upgrade::run(
        Command::new(&new_target)
            .args(["--headless", "system", "upgrade", "--yes", "--manifest"])
            .arg(&manifest_2)
            .env("HOME", &fixture.home)
            .env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1"),
        RUN,
    );
    assert_ne!(direct.returncode, 0, "a direct generation binary was accepted for update");
    // Headless reports the reason once, in its JSON error.
    assert!(direct.stdout().contains("managed hamn command symlink"), "{}", direct.stdout());
    let bad = fixture.work.join("bad-manifest.json");
    fs::write(&bad, "{").unwrap();
    assert_ne!(fixture.run_update(&bad, None).returncode, 0, "a modified manifest was accepted");
    fixture.assert_active_state(&new_target, &selection_2, "manifest rejection");

    // A retired schema v2 manifest is refused by its schema, with the
    // reinstall advice, before any download or state change.
    let v2 = fixture.work.join("v2-manifest.json");
    fs::write(&v2, fs::read_to_string(&manifest_2).unwrap().replace("\"schemaVersion\":3", "\"schemaVersion\":2"))
        .unwrap();
    let refused = fixture.run_update(&v2, None);
    assert_ne!(refused.returncode, 0, "a schema v2 manifest was accepted");
    assert!(
        refused.stdout().contains("manifest schema v2 is not supported; this Hamn reads only schema v3"),
        "{}",
        refused.stdout()
    );
    assert!(refused.stdout().contains("Reinstall with the official installer"), "{}", refused.stdout());
    fixture.assert_active_state(&new_target, &selection_2, "schema v2 rejection");

    // An installer failure occurs after both payloads are staged but before
    // either public pointer may change.
    build("0.0.3");
    let manifest_fail = fixture.release("0.0.3", "immutable guest image v0.0.3 failed", "-failed");
    let failed = fixture.run_update(&manifest_fail, Some("host-install"));
    assert_ne!(failed.returncode, 0, "a failing host installer was accepted");
    assert!(
        failed.stdout().contains("host install failed; prior binary and guest image selection were restored"),
        "{}",
        failed.stdout()
    );
    fixture.assert_active_state(&new_target, &selection_2, "host installer failure");

    // Interrupt the managed updater itself (the generation's own
    // executable), so SIGKILL reaches it rather than the command supervisor.
    let manifest_3 = fixture.release("0.0.3", "immutable guest image v0.0.3", "");
    let managed_bindir = fs::canonicalize(&fixture.bindir).unwrap();
    let managed_datadir = fs::canonicalize(&fixture.datadir).unwrap();
    let helper = |barrier: &Barrier| {
        let mut command = Command::new(&new_target);
        command
            .args(["__install-support", "update", "--bindir"])
            .arg(&managed_bindir)
            .arg("--datadir")
            .arg(&managed_datadir)
            .arg("--manifest")
            .arg(&manifest_3)
            .env("HOME", &fixture.home)
            .env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1")
            .stdin(Stdio::null());
        Group::spawn(barrier.apply(&mut command))
    };
    let term = Barrier::new(&fixture.work, "AFTER_HOST_INSTALL", "term");
    let child = helper(&term);
    term.await_ready(Duration::from_secs(60), "the TERM host cutover");
    child.signal(libc::SIGTERM);
    let stopped = child.finish(RUN);
    assert_eq!(stopped.returncode, 143, "TERM did not interrupt the update: {}", stopped.stderr());
    assert!(
        stopped.stderr().contains("interrupted by TERM; prior binary and guest image selection were restored"),
        "{}",
        stopped.stderr()
    );
    fixture.assert_active_state(&new_target, &selection_2, "TERM interruption");

    let kill = Barrier::new(&fixture.work, "AFTER_HOST_INSTALL", "kill");
    let child = helper(&kill);
    kill.await_ready(Duration::from_secs(60), "the SIGKILL host cutover");
    child.signal_group(libc::SIGKILL);
    let killed = child.finish(RUN);
    assert_eq!(killed.returncode, -libc::SIGKILL, "SIGKILL did not terminate the update");
    assert!(fixture.journal().exists(), "SIGKILL did not leave a durable recovery transaction");
    assert_ne!(fixture.active(), new_target, "SIGKILL did not reach the host cutover boundary");
    let start = upgrade::run(
        Command::new(fixture.command())
            .args(["--headless", "vm", "start", "--profile", "default", "--yes"])
            .env("HOME", &fixture.home),
        RUN,
    );
    assert_ne!(start.returncode, 0, "a pending update transaction allowed VM start");
    assert!(start.stderr().contains("interrupted upgrade recovery is pending"), "{}", start.stderr());

    // The next update recovers before it validates the new manifest; a
    // tampered manifest therefore proves recovery without another cutover.
    let recover = fixture.run_update(&bad, None);
    assert_ne!(recover.returncode, 0, "a recovered update accepted a modified manifest");
    assert!(
        recover.stderr().contains("recovered the previous binary and guest image selection"),
        "{}",
        recover.stderr()
    );
    fixture.assert_active_state(&new_target, &selection_2, "SIGKILL recovery");

    let result = fixture.run_update(&manifest_3, None);
    assert_eq!(result.returncode, 0, "{} {}", result.stdout(), result.stderr());
    let target_3 = fixture.active();
    assert_ne!(target_3, new_target, "the recovered updater could not perform a later update");
    assert_eq!(fixture.version(), "hamn 0.0.3\n");
    assert_ne!(fixture.selection_digest(), selection_2, "the later update did not change the guest image selection");
    assert!(fs::symlink_metadata(fixture.journal()).is_err());

    // Journals of Hamn 0.1.2 and earlier (v1) and pre-release builds (v2)
    // record no attempted generation: refused loudly and kept unchanged,
    // with no binary or selection change, pending or retired.
    let selection_3 = fixture.selection_digest();
    let cache = fixture.cache();
    for (version, state) in [
        (1, "version=1\nbootstrap=0\nselection=present\n"),
        (2, "version=2\nbootstrap=0\nselection=present\nhostMutation=1\n"),
    ] {
        for (name, pending) in [(".hamn-update-transaction", true), (".hamn-update-completed.Abc123", false)] {
            let journal = cache.join(name);
            fs::create_dir(&journal).unwrap();
            fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
            for (file, contents) in [
                ("state", state.as_bytes().to_vec()),
                ("attempt", b"Abc123\n".to_vec()),
                ("old-target", format!("{}\n", target_3.display()).into_bytes()),
                ("previous-selection", fs::read(cache.join("guest-image.json")).unwrap()),
                ("new-selection", b"{}\n".to_vec()),
            ] {
                fs::write(journal.join(file), contents).unwrap();
                fs::set_permissions(journal.join(file), fs::Permissions::from_mode(0o600)).unwrap();
            }
            let digests = |journal: &Path| -> Vec<String> {
                ["attempt", "new-selection", "old-target", "previous-selection", "state"]
                    .iter()
                    .map(|file| file_digest(&journal.join(file)))
                    .collect()
            };
            let before = digests(&journal);
            let result = fixture.run_update(&manifest_3, None);
            assert_ne!(result.returncode, 0, "a v{version} journal was accepted: {}", journal.display());
            let expected = if pending {
                format!(
                    "an interrupted update from Hamn 0.1.2 or earlier left a v{version} journal at {}, which this Hamn cannot recover",
                    journal.display()
                )
            } else {
                format!(
                    "a finished v{version} update journal from Hamn 0.1.2 or earlier remains at {}; move it aside",
                    journal.display()
                )
            };
            assert!(
                result.stdout().contains(&expected),
                "v{version} journal refusal is not explained: {}",
                result.stdout()
            );
            assert_eq!(digests(&journal), before, "refusing a v{version} journal changed it");
            assert_eq!(fixture.active(), target_3, "refusing a v{version} journal changed the installation");
            assert_eq!(fixture.selection_digest(), selection_3);
            fs::remove_dir_all(&journal).unwrap();
        }
    }
    let result = fixture.run_update(&manifest_3, None);
    assert_eq!(result.returncode, 0, "{} {}", result.stdout(), result.stderr());
    assert!(result.stderr().lines().any(|line| line == "Hamn 0.0.3 is up to date."), "{}", result.stderr());
}
