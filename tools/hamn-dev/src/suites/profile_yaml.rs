//! Strict C profile parsing through the public headless contract: private
//! atomic creation, rejected configuration leaves files unchanged, advanced
//! settings survive resource-only edits, malformed profiles (including the
//! removed managed-K3s `kubernetes` key) are refused, and deletion is soft.
//!
//! One flow, as in the script it replaces.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, MkdTemp};
use crate::support::hamn;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "profile-yaml",
        "strict profiles, advanced settings preservation and soft deletion",
        vec![case(
            "strict_profiles_advanced_settings_preservation_and_soft_deletion",
            strict_profiles_advanced_settings_preservation_and_soft_deletion,
        )],
        filters,
    )
}

/// Malformed profiles, each refused by `vm status` and `vm configure`
/// without a change to its file.
const CASES: &[(&str, &str)] = &[
    ("unknown-key", "cpus: 4\nunknown: true\n"),
    ("duplicate-key", "cpus: 4\ncpus: 5\n"),
    ("yaml-anchor", "cpus: &cpu 4\n"),
    ("yaml-alias", "cpus: *cpu\n"),
    ("yaml-tag", "cpus: !!int 4\n"),
    ("yaml-merge", "base: &base { cpus: 4 }\n<<: *base\n"),
    ("quoted-bool", "mountHome: \"true\"\n"),
    ("wrong-type", "mounts: true\n"),
    ("invalid-mount", "mounts:\n  - location: relative\n    mountPoint: /workspace\n    writable: false\n"),
    ("invalid-hook", "provision:\n  - command: echo ready\n    stage: invalid\n    timeoutSeconds: 60\n    mode: fail\n"),
    ("removed-network-key", "network:\n  mode: shared\n"),
    ("removed-kubernetes-key", "kubernetes:\n  enabled: true\n  version: v1.36.2+k3s1\n"),
    ("quoted-mount-inotify", "mountInotify: \"true\"\n"),
    ("no-writable-mount-inotify", "mountHome: false\nmountInotify: true\n"),
    ("invalid-docker-json", "docker:\n  daemonJson: \"[\"\n"),
    ("invalid-docker-key", "docker:\n  daemonJson: \"{\\\"containerd\\\":\\\"/other.sock\\\"}\"\n"),
];

struct Profiles {
    binary: PathBuf,
    root: PathBuf,
}

impl Profiles {
    /// `hamn --headless ARGS`: its envelope, whose `ok` matches the exit status.
    fn run(&self, arguments: &[&str]) -> Value {
        let mut command = Command::new(&self.binary);
        command.arg("--headless").args(arguments).env("HOME", &self.root);
        let result = api_fixtures::run(&mut command, None, Duration::from_secs(15));
        let value = result.json();
        assert_eq!(value["ok"].as_bool(), Some(result.success()), "{value}");
        value
    }

    fn ok(&self, arguments: &[&str]) -> bool {
        self.run(arguments)["ok"].as_bool().unwrap()
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(".hamn").join(name).join("config.yaml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
}

fn strict_profiles_advanced_settings_preservation_and_soft_deletion() {
    let directory = MkdTemp::new("hamn-yaml-");
    let root = directory.path();
    let profiles = Profiles { binary: hamn(), root: root.to_path_buf() };

    assert!(!profiles.ok(&["vm", "status"]));
    assert!(!root.join(".hamn").exists());
    assert!(!profiles.ok(&["vm", "create", "--profile", "test"]));
    assert!(!root.join(".hamn").exists());
    assert!(profiles.ok(&["vm", "create", "--profile", "test", "--cpu", "6", "--memory", "8", "--disk", "80", "--yes"]));
    let config = root.join(".hamn/test/config.yaml");
    assert_eq!(fs::metadata(&config).unwrap().permissions().mode() & 0o7777, 0o600);
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("cpus: 6") && text.contains("memoryMiB: 8192") && text.contains("diskGiB: 80"), "{text}");
    assert!(!text.contains("kubernetes:"), "{text}");
    let value = profiles.run(&["vm", "env", "--profile", "test"]);
    assert_eq!(value["data"]["DOCKER_HOST"], format!("unix://{}/.hamn/test/docker.sock", root.display()).as_str());
    for arguments in [
        &["--cpu", "0"][..],
        &["--memory", "0"],
        &["--disk", "0"],
        &["--cpu", "2", "--cpu", "3"],
        &["--network", "shared"],
        &["--runtime", "containerd"],
        &["--kubernetes", "true"],
    ] {
        let before = fs::read(&config).unwrap();
        assert!(!profiles.ok(&[&["vm", "configure", "--profile", "test", "--yes"][..], arguments].concat()), "{arguments:?}");
        assert_eq!(fs::read(&config).unwrap(), before, "{arguments:?}");
    }

    // Existing advanced settings are preserved by resource-only configuration.
    let share = root.join("share");
    fs::create_dir(&share).unwrap();
    let advanced = profiles.write(
        "advanced",
        &format!(
            "cpus: 2\nmountHome: false\nmountInotify: true\nrosetta: true\nnestedVirtualization: true\nsshAgent: true\n\
             docker:\n  daemonJson: '{{\"features\":{{\"buildkit\":true}}}}'\n\
             mounts:\n  - location: \"{}\"\n    mountPoint: /workspace\n    writable: true\n\
             provision:\n  - command: echo ready\n    stage: system\n    timeoutSeconds: 30\n    mode: fail\n",
            share.display()
        ),
    );
    assert!(profiles.ok(&["vm", "configure", "--profile", "advanced", "--cpu", "4", "--yes"]));
    let share_text = share.display().to_string();
    for required in [
        "mountInotify: true",
        "rosetta: true",
        "nestedVirtualization: true",
        "sshAgent: true",
        "echo ready",
        "/workspace",
        share_text.as_str(),
        "buildkit",
    ] {
        assert!(fs::read_to_string(&advanced).unwrap().contains(required), "{required}");
    }

    // CASES are populated from the existing strict-parser regression fixtures.
    for (name, text) in CASES {
        let path = profiles.write(name, text);
        let before = fs::read(&path).unwrap();
        let value = profiles.run(&["vm", "status", "--profile", name]);
        assert_eq!(value["ok"], false, "{name} {value}");
        assert_eq!(fs::read(&path).unwrap(), before, "{name}");
        assert!(!profiles.ok(&["vm", "configure", "--profile", name, "--cpu", "4", "--yes"]), "{name}");
        assert_eq!(fs::read(&path).unwrap(), before, "{name}");
    }

    for (name, _) in CASES {
        fs::remove_file(root.join(".hamn").join(name).join("config.yaml")).unwrap();
    }
    assert!(profiles.ok(&["vm", "create", "--profile", "deleted", "--yes"]));
    let disk = root.join(".hamn/deleted/disk.img");
    fs::write(&disk, b"Docker data sentinel").unwrap();
    assert!(!profiles.ok(&["vm", "delete", "--profile", "deleted"]));
    assert!(profiles.ok(&["vm", "delete", "--profile", "deleted", "--yes"]));
    assert_eq!(fs::read(&disk).unwrap(), b"Docker data sentinel");
    assert!(disk.parent().unwrap().join("deleted").is_file());
    let rows = profiles.run(&["vm", "list"]);
    let rows = rows["data"].as_array().unwrap_or_else(|| panic!("vm list rows: {rows}"));
    assert!(rows.iter().all(|row| *row.get("name").expect("row name") != "deleted"), "{rows:?}");
}
