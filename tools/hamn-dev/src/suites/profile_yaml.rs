//! Strict C profile parsing through the public headless contract: private
//! atomic creation, rejected configuration leaves files unchanged, advanced
//! settings survive resource-only edits, malformed profiles (including the
//! removed managed-K3s `kubernetes` key) are refused by every operation with
//! the rule they break, and deletion is soft.
//!
//! One flow, as in the script it replaces.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, MkdTemp};
use crate::support::hamn;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
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

/// Malformed profiles: name, `config.yaml`, and the rule of the parser that
/// the file breaks. Every operation that needs the stored configuration
/// refuses each of them with that rule and without a change to the file.
const CASES: &[(&str, &str, &str)] = &[
    ("unknown-key", "cpus: 4\nunknown: true\n", "unknown configuration key: unknown"),
    ("duplicate-key", "cpus: 4\ncpus: 5\n", "duplicate or invalid configuration key"),
    ("yaml-anchor", "cpus: &cpu 4\n", "YAML anchors and tags are not supported"),
    ("yaml-alias", "cpus: *cpu\n", "YAML aliases are not supported"),
    ("yaml-tag", "cpus: !!int 4\n", "YAML anchors and tags are not supported"),
    // The file breaks three rules. The parser reads a value before it looks
    // at its key, so the anchor is what it meets first.
    ("yaml-merge", "base: &base { cpus: 4 }\n<<: *base\n", "YAML anchors and tags are not supported"),
    ("quoted-bool", "mountHome: \"true\"\n", "invalid scalar value"),
    ("wrong-type", "mounts: true\n", "expected a sequence"),
    (
        "invalid-mount",
        "mounts:\n  - location: relative\n    mountPoint: /workspace\n    writable: false\n",
        "a mount location must be a normalized absolute path",
    ),
    (
        "invalid-hook",
        "provision:\n  - command: echo ready\n    stage: invalid\n    timeoutSeconds: 60\n    mode: fail\n",
        "a provision stage must be system, user, after-boot or ready",
    ),
    ("removed-network-key", "network:\n  mode: shared\n", "unknown configuration key: network"),
    (
        "removed-kubernetes-key",
        "kubernetes:\n  enabled: true\n  version: v1.36.2+k3s1\n",
        "unknown configuration key: kubernetes",
    ),
    ("quoted-mount-inotify", "mountInotify: \"true\"\n", "invalid scalar value"),
    (
        "no-writable-mount-inotify",
        "mountHome: false\nmountInotify: true\n",
        "mountInotify requires a writable share",
    ),
    ("invalid-docker-json", "docker:\n  daemonJson: \"[\"\n", DAEMON_JSON_RULE),
    (
        "invalid-docker-key",
        "docker:\n  daemonJson: \"{\\\"containerd\\\":\\\"/other.sock\\\"}\"\n",
        DAEMON_JSON_RULE,
    ),
];

const DAEMON_JSON_RULE: &str =
    "docker.daemonJson must be one JSON object that leaves the settings Hamn manages alone";

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

    /// The error of a refused `hamn --headless ARGS`: its code and message.
    fn refusal(&self, arguments: &[&str]) -> (String, String) {
        let value = self.run(arguments);
        assert_eq!(value["ok"], false, "{arguments:?} {value}");
        assert!(value["data"].is_null(), "{arguments:?} {value}");
        let field = |key: &str| {
            value["error"][key].as_str().unwrap_or_else(|| panic!("{arguments:?} {value}")).to_owned()
        };
        (field("code"), field("message"))
    }

    /// The profile directory is private, as the profiles Hamn creates are:
    /// the parser looks at the file only in such a directory, whatever the
    /// umask of the test run.
    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(".hamn").join(name).join("config.yaml");
        fs::DirBuilder::new().recursive(true).mode(0o700).create(path.parent().unwrap()).unwrap();
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
    // An operation that reads the stored configuration says which rule the
    // file breaks: the code is the one of its kind of request, the message
    // names the profile and the rule, and the file keeps its bytes.
    let archive = root.join("diagnostics.tar");
    let archive_text = api_fixtures::utf8(&archive);
    for (name, text, rule) in CASES {
        let path = profiles.write(name, text);
        let before = fs::read(&path).unwrap();
        let unreadable = format!("cannot read the configuration of profile {name}: {rule}");
        for (code, arguments) in [
            ("profileUnavailable", &["vm", "status", "--profile", name][..]),
            ("profileUnavailable", &["vm", "env", "--profile", name]),
            // A Docker request finds its socket through the profile's status.
            ("profileUnavailable", &["docker", "containers", "list", "--profile", name]),
            ("operationFailed", &["vm", "configure", "--profile", name, "--cpu", "4", "--yes"]),
            ("operationFailed", &["vm", "stop", "--profile", name, "--yes"]),
            ("operationFailed", &["vm", "diagnostics", "--profile", name, "--path", archive_text, "--yes"]),
            // A known refusal, not the unknown outcome of a worker that
            // ended. `--disk 1` stops a start that got past the read before
            // it saves the profile or prepares an image.
            ("operationFailed", &["vm", "start", "--profile", name, "--disk", "1", "--yes"]),
            ("operationFailed", &["vm", "delete", "--profile", name, "--yes"]),
        ] {
            assert_eq!(profiles.refusal(arguments), (code.to_owned(), unreadable.clone()), "{arguments:?}");
            assert_eq!(fs::read(&path).unwrap(), before, "{arguments:?}");
        }
        assert!(!archive.exists(), "{name}");
        // The refused start recorded no operation and the refused delete
        // marked nothing: the directory holds the file alone.
        let directory = path.parent().unwrap();
        let entries: Vec<_> = fs::read_dir(directory).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(entries, ["config.yaml"], "{name}");
    }
    // A name that no profile has is told apart from a file that cannot be
    // read, and asking about it creates no profile.
    for (code, arguments) in [
        ("profileUnavailable", &["vm", "status", "--profile", "absent"][..]),
        ("operationFailed", &["vm", "configure", "--profile", "absent", "--cpu", "4", "--yes"]),
        ("operationFailed", &["vm", "stop", "--profile", "absent", "--yes"]),
    ] {
        assert_eq!(
            profiles.refusal(arguments),
            (code.to_owned(), "profile absent does not exist".to_owned()),
            "{arguments:?}"
        );
        assert!(!root.join(".hamn/absent").exists(), "{arguments:?}");
    }

    for (name, _, _) in CASES {
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
