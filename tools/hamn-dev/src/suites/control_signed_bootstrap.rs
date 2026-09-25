//! The public Rust start/retry path with an owned updater fixture: the
//! worker runs the invoked `hamn` to prepare the signed guest image, and the
//! frontend retries through that same installation exactly once, keeping an
//! earlier uncertain outcome.
//!
//! This tests the handoff protocol, not signature verification or a physical
//! VM. The fixture populates only its private cache and ends the retry
//! before VM work.
use crate::runner::{self, Case, case};
use crate::support::api_fixtures::{self, MkdTemp, py_json, truthy, utf8};
use crate::support::{hamn, tui::install_fixture};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let cases: Vec<Case> = [false, true]
        .into_iter()
        .map(|previous_unknown| {
            case(format!("public_start_retries_the_installed_binary_once/previous_unknown={previous_unknown}"), move || {
                public_start_retries_the_installed_binary_once(previous_unknown)
            })
        })
        .collect();
    runner::run(
        "control-signed-bootstrap",
        "public start retries the installed binary exactly once after image preparation; prior uncertainty retained",
        cases,
        filters,
    )
}

fn write_private(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn public_start_retries_the_installed_binary_once(previous_unknown: bool) {
    let directory = MkdTemp::new_in(Path::new("/tmp"), "hamn-bootstrap-public-");
    let root = directory.path();
    let updater = root.join("hamn");
    install_fixture(root, "hamn");
    if previous_unknown {
        let profile = root.join(".hamn/bootstrap");
        fs::create_dir_all(&profile).unwrap();
        let record = json!({"schemaVersion": 1, "operationId": "a".repeat(32), "status": "outcomeUnknown",
            "pid": 1, "startSec": 0, "startUsec": 0, "executableUuid": "0".repeat(32)});
        write_private(&profile.join("operation.json"), &py_json(&record));
    }
    // argv[0] identifies the installation that the real worker invokes for
    // update, then the public Rust frontend uses for its one retry.
    let mut command = Command::new(hamn());
    command
        .arg0(&updater)
        .args(["--headless", "vm", "start", "--profile", "bootstrap", "--yes"])
        .env("HOME", root)
        .env("HAMN_DEV_FIXTURE", "control-signed-bootstrap");
    let result = api_fixtures::run(&mut command, None, Duration::from_secs(20));
    assert!(result.success(), "{result:?}");
    let envelope = api_fixtures::parse(result.stdout.lines().last().expect("an envelope line"));
    assert_eq!(envelope["data"]["bootstrapRetry"], json!(true), "{envelope}");
    let calls: Vec<Value> = fs::read_to_string(root.join("calls")).unwrap().lines().map(api_fixtures::parse).collect();
    assert_eq!(calls, [json!(["--headless", "system", "update", "--yes"]), json!(["__core-worker", utf8(&updater)])], "{calls:?}");
    let record = api_fixtures::parse(&fs::read_to_string(root.join(".hamn/bootstrap/operation.json")).unwrap());
    assert!(record["status"] == "restartRequired" && record["exitCode"] == 3, "{record}");
    assert!(record["phase"] == "signed-image-ready" && record["error"] == "", "{record}");
    assert_eq!(truthy(record.get("recoveryRequired")), previous_unknown, "{record}");
    let mut command = Command::new(hamn());
    command.args(["--headless", "vm", "status", "--profile", "bootstrap"]).env("HOME", root).env("HAMN_DEV_FIXTURE", "control-signed-bootstrap");
    let status = api_fixtures::run(&mut command, None, Duration::from_secs(10));
    assert!(status.success(), "{}", status.stderr);
    let data = status.json()["data"].clone();
    let expected = if previous_unknown { "recoveryRequired" } else { "unavailable" };
    assert_eq!(data["dockerStatus"], expected, "{data}");
    let profile = root.join(".hamn/bootstrap");
    let names: Vec<String> = fs::read_dir(&profile).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(!names.iter().any(|name| name.ends_with(".sock")), "{names:?}");
    assert!(!profile.join("vmrun.pid").exists());
    assert!(!names.iter().any(|name| name.ends_with(".img")), "{names:?}");
}

/// The installed `hamn` as the worker and frontend invoke it: `--headless
/// system update --yes` fills the private image cache, and the retried
/// `__core-worker` ends the start before VM work. Every call is appended to
/// `$HOME/calls`.
pub fn updater_fixture(_program: &str, args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let mut calls = OpenOptions::new().create(true).append(true).open(root.join("calls")).unwrap();
    writeln!(calls, "{}", py_json(&args)).unwrap();
    if args == ["--headless", "system", "update", "--yes"] {
        let cache = root.join(".hamn/cache");
        fs::create_dir_all(&cache).unwrap();
        let digest = "0123456789abcdef".repeat(4);
        let name = format!("hamn-guest-{digest}.img");
        let selection = py_json(&json!({"schemaVersion": 1, "file": name, "sha256": digest}));
        for (file, text) in [(name.clone(), "fixture"), (format!("{name}.verified"), digest.as_str()), ("guest-image.json".into(), &selection)] {
            write_private(&cache.join(file), text);
        }
    } else if args.first().map(String::as_str) == Some("__core-worker") {
        let request: Value = serde_json::from_reader(std::io::stdin()).expect("worker request");
        assert!(request["words"] == json!(["vm", "start"]) && request["profile"] == "bootstrap", "{request}");
        println!("{}", py_json(&json!({"Ok": {"bootstrapRetry": true}})));
    } else {
        panic!("{args:?}");
    }
    ExitCode::SUCCESS
}
