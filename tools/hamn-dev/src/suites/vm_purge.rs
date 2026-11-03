//! `vm delete --purge` through the headless contract of the real binary,
//! with an isolated HOME: a confirmed purge deletes one profile directory
//! and nothing it links to, a repeat or a purge of a name that never existed
//! changes nothing, a VM not known to be stopped or a busy profile keeps
//! everything, and `vm list` removes what an interrupted purge left. The soft
//! delete stays as it was. Never starts a VM; tests/host/test_profile_purge.c
//! interrupts a real purge.
use crate::runner::{self, case};
use crate::support::api_fixtures;
use crate::support::hamn;
use crate::support::tmp::TempDir;
use serde_json::{Value, json};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "vm-purge",
        "a confirmed purge deletes one profile directory, once, or keeps it",
        vec![
            case("a_purge_deletes_the_profile_and_a_repeat_changes_nothing", a_purge_deletes_the_profile_and_a_repeat_changes_nothing),
            case("a_name_that_never_existed_is_already_absent_without_a_trace", a_name_that_never_existed_is_already_absent_without_a_trace),
            case("a_purge_needs_confirmation_and_belongs_to_vm_delete", a_purge_needs_confirmation_and_belongs_to_vm_delete),
            case("a_vm_not_known_to_be_stopped_keeps_the_profile", a_vm_not_known_to_be_stopped_keeps_the_profile),
            case("a_busy_profile_is_refused_and_kept", a_busy_profile_is_refused_and_kept),
            case("what_an_interrupted_purge_left_is_hidden_and_removed", what_an_interrupted_purge_left_is_hidden_and_removed),
            case("a_soft_delete_keeps_the_disk_for_a_start_to_restore", a_soft_delete_keeps_the_disk_for_a_start_to_restore),
        ],
        filters,
    )
}

/// The binary under test with a private HOME and created profiles.
struct Home {
    directory: TempDir,
}

impl Home {
    fn new(profiles: &[&str]) -> Self {
        let home = Self { directory: TempDir::new("hamn-purge-") };
        for profile in profiles {
            let created = home.run(&["vm", "create", "--profile", profile, "--yes"]);
            assert_eq!(created["ok"], true, "{created}");
        }
        home
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn hamn(&self) -> PathBuf {
        self.path().join(".hamn")
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

    fn purge(&self, profile: &str) -> (Value, String) {
        self.call(&["vm", "delete", "--profile", profile, "--purge", "--yes"])
    }

    fn listed(&self) -> Vec<String> {
        let value = self.run(&["vm", "list"]);
        assert_eq!(value["ok"], true, "{value}");
        value["data"].as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap().to_owned()).collect()
    }

    /// A disk and a log in the profile, and a link to a directory outside
    /// ~/.hamn that holds `keep`.
    fn plant(&self, profile: &str) {
        let directory = self.hamn().join(profile);
        fs::create_dir_all(directory.join("logs")).unwrap();
        private_file(&directory.join("disk.img"), "disk\n");
        private_file(&directory.join("logs/serial.log"), "boot\n");
        let outside = self.path().join(format!("outside-{profile}"));
        fs::create_dir(&outside).unwrap();
        private_file(&outside.join("keep"), "keep\n");
        std::os::unix::fs::symlink(&outside, directory.join("shared")).unwrap();
    }

    /// Every path under HOME with its kind and size, sorted.
    fn tree(&self) -> Vec<String> {
        let mut paths = Vec::new();
        walk(self.path(), self.path(), &mut paths);
        paths.sort();
        paths
    }
}

fn walk(root: &Path, directory: &Path, paths: &mut Vec<String>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let relative = path.strip_prefix(root).unwrap().display().to_string();
        paths.push(format!("{relative} {:?} {}", metadata.file_type(), metadata.len()));
        if metadata.is_dir() {
            walk(root, &path, paths);
        }
    }
}

fn private_file(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn failure(value: &Value, code: &str, message: &str) {
    assert!(value["ok"] == false && value["error"]["code"] == code, "{value}");
    assert!(value["error"]["message"].as_str().is_some_and(|text| text.contains(message)), "{value}");
}

fn a_purge_deletes_the_profile_and_a_repeat_changes_nothing() {
    let home = Home::new(&["work", "other"]);
    home.plant("work");
    home.plant("other");

    let (value, stderr) = home.purge("work");
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["target"]["profile"], "work", "{value}");
    assert_eq!(value["data"], json!({"deleted": true, "purged": true, "alreadyAbsent": false}));
    assert!(stderr.contains("deleted profile work with its disk and Docker data"), "{stderr}");
    assert!(!home.hamn().join("work").exists());
    assert!(home.path().join("outside-work/keep").exists());
    assert!(home.hamn().join("other/disk.img").exists());
    assert_eq!(home.listed(), ["other"]);
    let status = home.run(&["vm", "status", "--profile", "work"]);
    failure(&status, "profileUnavailable", "profile work does not exist");

    let before = home.tree();
    let (again, stderr) = home.purge("work");
    assert_eq!(again["data"], json!({"deleted": true, "purged": true, "alreadyAbsent": true}));
    assert!(stderr.contains("profile work does not exist; nothing to delete"), "{stderr}");
    assert_eq!(home.tree(), before);
}

fn a_name_that_never_existed_is_already_absent_without_a_trace() {
    let empty = Home::new(&[]);
    let (value, _) = empty.purge("ghost");
    assert_eq!(value["data"]["alreadyAbsent"], true, "{value}");
    assert!(!empty.hamn().exists());

    let home = Home::new(&["work"]);
    let before = home.tree();
    let (value, _) = home.purge("ghost");
    assert_eq!(value["data"]["alreadyAbsent"], true, "{value}");
    assert_eq!(home.tree(), before);
}

fn a_purge_needs_confirmation_and_belongs_to_vm_delete() {
    let home = Home::new(&["work"]);
    home.plant("work");
    let before = home.tree();
    for (arguments, reason) in [
        (&["vm", "delete", "--profile", "work", "--purge"][..], "mutation requires --yes"),
        (&["vm", "delete", "--purge", "--yes"][..], "an explicit --profile is required"),
        (&["vm", "reset", "--profile", "work", "--purge", "--yes"][..], "--purge is only supported for vm delete"),
    ] {
        failure(&home.run(arguments), "invalidRequest", reason);
    }
    assert_eq!(home.tree(), before);

    let capabilities = home.run(&["capabilities"]);
    let delete = capabilities["data"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["name"] == "vm delete")
        .unwrap_or_else(|| panic!("{capabilities}"))
        .clone();
    assert_eq!(delete["mutates"], true, "{delete}");
    assert_eq!(delete["arguments"]["required"], json!(["yes", "profile"]), "{delete}");
    let purge = &delete["arguments"]["properties"]["purge"];
    assert_eq!(purge["type"], "boolean", "{delete}");
    assert!(purge["description"].as_str().is_some_and(|text| text.contains("Permanently delete")), "{delete}");
}

/// An identity file that cannot be read makes the VM's ownership uncertain:
/// stop refuses to signal it, and the purge deletes nothing.
fn a_vm_not_known_to_be_stopped_keeps_the_profile() {
    let home = Home::new(&["work"]);
    home.plant("work");
    private_file(&home.hamn().join("work/vmrun.identity"), "not an identity\n");
    let before = home.tree();
    let (value, stderr) = home.purge("work");
    failure(&value, "outcomeUnknown", "refusing to delete while VM or host port-forward ownership is uncertain");
    assert!(stderr.contains("unverified vmrun"), "{stderr}");
    assert_eq!(home.tree(), before);
}

fn a_busy_profile_is_refused_and_kept() {
    let home = Home::new(&["work"]);
    home.plant("work");
    let lock = fs::OpenOptions::new().read(true).write(true).open(home.hamn().join(".work-mutation.lock")).unwrap();
    // SAFETY: the descriptor belongs to `lock`, which outlives the call.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    let before = home.tree();
    let (value, _) = home.purge("work");
    failure(&value, "operationFailed", "another work profile mutation is running");
    assert_eq!(home.tree(), before);

    drop(lock);
    let (value, _) = home.purge("work");
    assert_eq!(value["data"]["alreadyAbsent"], false, "{value}");
    assert!(!home.hamn().join("work").exists());
}

/// The state an interrupted purge leaves: the renamed tree, which no
/// listing shows and the next list or purge removes. Directories of other
/// names stay.
fn what_an_interrupted_purge_left_is_hidden_and_removed() {
    let home = Home::new(&["work"]);
    let left = home.hamn().join(".purge-old-0123456789abcdef");
    fs::create_dir(&left).unwrap();
    fs::write(left.join("config.yaml"), "cpus: 2\n").unwrap();
    private_file(&left.join("disk.img"), "disk\n");
    let other = home.hamn().join(".purge-old-not-hex");
    fs::create_dir(&other).unwrap();

    let (value, stderr) = home.call(&["vm", "list"]);
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(home.listed(), ["work"]);
    assert!(stderr.contains("removed") && stderr.contains(".purge-old-0123456789abcdef"), "{stderr}");
    assert!(!left.exists() && other.exists());

    fs::create_dir(&left).unwrap();
    let (value, _) = home.purge("ghost");
    assert_eq!(value["data"]["alreadyAbsent"], true, "{value}");
    assert!(!left.exists() && other.exists());
}

fn a_soft_delete_keeps_the_disk_for_a_start_to_restore() {
    let home = Home::new(&["work"]);
    home.plant("work");
    let (value, _) = home.call(&["vm", "delete", "--profile", "work", "--yes"]);
    assert_eq!(value["data"], json!({"deleted": true}), "{value}");
    for file in ["config.yaml", "disk.img", "logs/serial.log", "deleted"] {
        assert!(home.hamn().join("work").join(file).exists(), "{file}");
    }
    assert!(home.listed().is_empty());

    // A start rejected before any VM work still brings the profile back.
    let rejected = home.run(&["vm", "start", "--profile", "work", "--disk", "1", "--yes"]);
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(home.listed(), ["work"]);
    assert_eq!(fs::read_to_string(home.hamn().join("work/disk.img")).unwrap(), "disk\n");

    // A purge deletes a soft-deleted profile as well.
    let (value, _) = home.call(&["vm", "delete", "--profile", "work", "--yes"]);
    assert_eq!(value["ok"], true, "{value}");
    let (value, _) = home.purge("work");
    assert_eq!(value["data"]["alreadyAbsent"], false, "{value}");
    assert!(!home.hamn().join("work").exists() && home.path().join("outside-work/keep").exists());
}
