//! A start rejected before any VM work (a disk shrink) keeps the stopped VM
//! and its configuration, reports a known failure, and does not erase an
//! earlier unresolved operation. Never starts a VM.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, MkdTemp, py_json};
use crate::support::hamn;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "start-preflight",
        "rejected start preserves stopped VM/configuration and reports known failure",
        vec![case("rejected_start_preserves_stopped_vm_and_configuration", rejected_start_preserves_stopped_vm_and_configuration)],
        filters,
    )
}

/// `hamn --headless WORDS`, which must exit with `code`; its JSON envelope.
fn call(root: &Path, words: &[&str], code: i32) -> Value {
    let mut command = Command::new(hamn());
    command.arg("--headless").args(words).env("HOME", root);
    let result = api_fixtures::run(&mut command, None, Duration::from_secs(15));
    assert_eq!(result.status.code(), Some(code), "{result:?}");
    result.json()
}

fn rejected_start_preserves_stopped_vm_and_configuration() {
    let temporary = MkdTemp::new("hamn-start-preflight-");
    let root = temporary.path();
    let shrink = ["vm", "start", "--profile", "fixture", "--disk", "10", "--yes"];

    call(root, &["vm", "create", "--profile", "fixture", "--disk", "20", "--yes"], 0);
    let profile = root.join(".hamn/fixture");
    let before = fs::read(profile.join("config.yaml")).unwrap();
    let result = call(root, &shrink, 1);
    assert_eq!(result["error"]["code"], "operationFailed", "{result}");
    assert!(result["error"]["message"].as_str().is_some_and(|message| message.contains("disk size cannot shrink")), "{result}");
    let state = call(root, &["vm", "status", "--profile", "fixture"], 0)["data"].clone();
    assert!(state["state"] == "stopped" && state["dockerStatus"] == "unavailable", "{state}");
    assert_eq!(state["lastOperation"]["status"], "failed", "{state}");
    assert_eq!(state["lastOperation"]["startedVm"], json!(false), "{state}");
    assert!(state["diskGiB"] == 20 && fs::read(profile.join("config.yaml")).unwrap() == before, "{state}");
    assert!(!["vmrun.pid", "vmrun.identity", "docker.sock"].iter().any(|file| profile.join(file).exists()));
    // A rejected command must not erase an unresolved earlier operation.
    let record = profile.join("operation.json");
    let mut value = api_fixtures::parse(&fs::read_to_string(&record).unwrap());
    value["status"] = json!("outcomeUnknown");
    fs::write(&record, py_json(&value)).unwrap();
    let result = call(root, &shrink, 1);
    assert_eq!(result["error"]["code"], "outcomeUnknown", "{result}");
    let state = call(root, &["vm", "status", "--profile", "fixture"], 0)["data"].clone();
    assert_eq!(state["dockerStatus"], "recoveryRequired", "{state}");
    assert_eq!(fs::read(profile.join("config.yaml")).unwrap(), before);
}
