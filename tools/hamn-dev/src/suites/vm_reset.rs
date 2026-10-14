//! `vm reset` through the headless contract of the real binary, with an
//! isolated HOME: a confirmed reset deletes one profile's VM disk and what
//! belongs to that disk, keeps the rest of the profile, settles an earlier
//! unknown outcome, and can be repeated; a VM that is not known to be
//! stopped keeps its disk, and a removal that fails is finished by a retry.
//! Never starts a VM.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, py_json};
use crate::support::hamn;
use crate::support::tmp::TempDir;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "vm-reset",
        "a confirmed reset deletes only one profile's VM disk, or keeps it",
        vec![
            case("a_reset_deletes_the_disk_and_keeps_the_profile", a_reset_deletes_the_disk_and_keeps_the_profile),
            case("a_reset_needs_confirmation_and_an_existing_profile", a_reset_needs_confirmation_and_an_existing_profile),
            case("a_reset_settles_an_earlier_unknown_outcome", a_reset_settles_an_earlier_unknown_outcome),
            case("a_vm_not_known_to_be_stopped_keeps_its_disk", a_vm_not_known_to_be_stopped_keeps_its_disk),
            case("a_failed_removal_is_finished_by_a_retry", a_failed_removal_is_finished_by_a_retry),
        ],
        filters,
    )
}

/// What belongs to one VM disk, in the order a reset removes it.
const DISK_FILES: [&str; 4] = ["guest-deployment.version", "efi-vars.bin", "disk.img", "guest-filesystem.json"];
/// What a reset keeps: the settings, the identity of the VM and its logs.
const KEPT_FILES: [&str; 6] = ["config.yaml", "id_ed25519", "id_ed25519.pub", "mac-addr", "seed.iso", "logs/serial.log"];

/// The binary under test with a private HOME and a created profile `work`.
struct Home {
    directory: TempDir,
}

impl Home {
    fn new() -> Self {
        let home = Self { directory: TempDir::new("hamn-reset-") };
        let created = home.run(&["vm", "create", "--profile", "work", "--yes"]);
        assert_eq!(created["ok"], true, "{created}");
        home
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn profile(&self) -> PathBuf {
        self.path().join(".hamn/work")
    }

    /// `hamn --headless ARGUMENTS`: its envelope and standard error. The
    /// exit status matches `ok`.
    fn call(&self, arguments: &[&str]) -> (Value, String) {
        let mut command = Command::new(hamn());
        command.arg("--headless").args(arguments).env("HOME", self.path()).current_dir(self.path());
        let result = api_fixtures::run(&mut command, None, Duration::from_secs(60));
        let value = result.json();
        assert_eq!(result.status.code(), Some(if value["ok"] == true { 0 } else { 1 }), "{result:?}");
        (value, result.stderr)
    }

    fn run(&self, arguments: &[&str]) -> Value {
        self.call(arguments).0
    }

    fn reset(&self) -> (Value, String) {
        self.call(&["vm", "reset", "--profile", "work", "--yes"])
    }

    fn status(&self) -> Value {
        let value = self.run(&["vm", "status", "--profile", "work"]);
        assert_eq!(value["ok"], true, "{value}");
        value["data"].clone()
    }

    /// The files a used VM leaves: a disk with a recorded damage, its EFI
    /// variables and deployment marker, and what a reset keeps.
    fn plant(&self) {
        let profile = self.profile();
        fs::create_dir_all(profile.join("logs")).unwrap();
        for (name, text) in [
            ("disk.img", "disk\n"),
            ("efi-vars.bin", "efi\n"),
            ("guest-deployment.version", "schemaVersion=6\nfingerprint=sha256:0\n"),
            ("id_ed25519", "private\n"),
            ("id_ed25519.pub", "public\n"),
            ("mac-addr", "52:54:00:00:00:01\n"),
            ("seed.iso", "seed\n"),
            ("logs/serial.log", "boot\n"),
        ] {
            private_file(&profile.join(name), text);
        }
        let disk = fs::metadata(profile.join("disk.img")).unwrap();
        private_file(
            &profile.join("guest-filesystem.json"),
            &json!({"schemaVersion": 1, "state": "unreadable",
                "detail": "the guest cannot read its disk: /bin/bash: Input/output error",
                "observedAt": 1790000000, "disk": format!("{}:{}", disk.dev(), disk.ino())})
            .to_string(),
        );
    }

    fn kept(&self) -> Vec<Vec<u8>> {
        KEPT_FILES.iter().map(|name| fs::read(self.profile().join(name)).unwrap()).collect()
    }

    fn present(&self) -> Vec<&'static str> {
        DISK_FILES.iter().copied().filter(|name| fs::symlink_metadata(self.profile().join(name)).is_ok()).collect()
    }

    fn set_operation_status(&self, status: &str) {
        let record = self.profile().join("operation.json");
        let mut value = api_fixtures::parse(&fs::read_to_string(&record).unwrap());
        value["status"] = json!(status);
        fs::write(&record, py_json(&value)).unwrap();
    }
}

fn private_file(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn a_reset_deletes_the_disk_and_keeps_the_profile() {
    let home = Home::new();
    home.plant();
    assert_eq!(home.status()["guestFilesystem"]["state"], "unreadable");
    let kept = home.kept();

    let (value, stderr) = home.reset();
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["target"]["profile"], "work", "{value}");
    let data = &value["data"];
    assert!(data["state"] == "stopped" && data["dockerStatus"] == "unavailable", "{data}");
    assert_eq!(data["guestFilesystem"], Value::Null, "{data}");
    assert!(data["lastOperation"]["operation"] == "vm reset" && data["lastOperation"]["status"] == "completed", "{data}");
    assert!(home.present().is_empty(), "{:?}", home.present());
    assert_eq!(home.kept(), kept);
    assert!(stderr.contains("deleted the VM disk of profile work"), "{stderr}");

    // A repeat finds nothing to delete and succeeds.
    let (again, stderr) = home.reset();
    assert_eq!(again["ok"], true, "{again}");
    assert!(stderr.contains("profile work had no VM disk"), "{stderr}");
    assert_eq!(home.kept(), kept);
}

fn a_reset_needs_confirmation_and_an_existing_profile() {
    let home = Home::new();
    home.plant();
    for (arguments, reason) in [
        (&["vm", "reset", "--profile", "work"][..], "mutation requires --yes"),
        (&["vm", "reset", "--yes"][..], "an explicit --profile is required"),
    ] {
        let value = home.run(arguments);
        assert!(value["ok"] == false && value["error"]["code"] == "invalidRequest", "{value}");
        assert!(value["error"]["message"].as_str().is_some_and(|message| message.contains(reason)), "{value}");
    }
    assert_eq!(home.present(), DISK_FILES.to_vec());

    let value = home.run(&["vm", "reset", "--profile", "ghost", "--yes"]);
    assert_eq!(value["ok"], false, "{value}");
    assert!(value["error"]["message"].as_str().is_some_and(|message| message.contains("profile ghost does not exist")), "{value}");
    assert!(!home.path().join(".hamn/ghost").exists());

    // The confirmation names the loss; capabilities list the operation as a
    // mutation that takes a profile.
    let capabilities = home.run(&["capabilities"]);
    let reset = capabilities["data"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["name"] == "vm reset")
        .unwrap_or_else(|| panic!("{capabilities}"))
        .clone();
    assert_eq!(reset["mutates"], true, "{reset}");
    assert_eq!(reset["arguments"]["required"], json!(["yes", "profile"]), "{reset}");
    assert!(reset["impact"].as_str().is_some_and(|impact| impact.contains("permanently delete")), "{reset}");
}

fn a_reset_settles_an_earlier_unknown_outcome() {
    let home = Home::new();
    // A start rejected before any VM work leaves a record to mark.
    let rejected = home.run(&["vm", "start", "--profile", "work", "--disk", "1", "--yes"]);
    assert_eq!(rejected["ok"], false, "{rejected}");
    home.set_operation_status("outcomeUnknown");
    assert_eq!(home.status()["dockerStatus"], "recoveryRequired");
    home.plant();

    let (value, _) = home.reset();
    assert_eq!(value["ok"], true, "{value}");
    let status = home.status();
    assert!(status["dockerStatus"] == "unavailable" && status["lastOperation"]["status"] == "completed", "{status}");
}

/// An identity file that cannot be read makes the VM's ownership uncertain:
/// stop refuses to signal it, and the reset keeps everything.
fn a_vm_not_known_to_be_stopped_keeps_its_disk() {
    let home = Home::new();
    home.plant();
    private_file(&home.profile().join("vmrun.identity"), "not an identity\n");
    let (value, _) = home.reset();
    assert_eq!(value["ok"], false, "{value}");
    assert_eq!(value["error"]["code"], "outcomeUnknown", "{value}");
    assert!(value["error"]["message"].as_str().is_some_and(|message| message.contains("unverified vmrun")), "{value}");
    assert_eq!(home.present(), DISK_FILES.to_vec());
    assert_eq!(home.status()["guestFilesystem"]["state"], "unreadable");
}

/// A disk.img that cannot be removed stops the reset after the files before
/// it; the record of the damage stays until a retry removes the disk.
fn a_failed_removal_is_finished_by_a_retry() {
    let home = Home::new();
    home.plant();
    let disk = home.profile().join("disk.img");
    fs::remove_file(&disk).unwrap();
    fs::create_dir(&disk).unwrap();
    fs::write(disk.join("held"), "x").unwrap();

    let (value, _) = home.reset();
    assert!(value["ok"] == false && value["error"]["code"] == "operationFailed", "{value}");
    let message = value["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("disk.img") && message.contains("run vm reset again"), "{value}");
    assert_eq!(home.present(), ["disk.img", "guest-filesystem.json"]);
    assert_eq!(home.status()["lastOperation"]["status"], "failed");

    fs::remove_dir_all(&disk).unwrap();
    let (value, _) = home.reset();
    assert_eq!(value["ok"], true, "{value}");
    assert!(home.present().is_empty(), "{:?}", home.present());
}
