//! A start rejected before any VM work (a disk shrink, an unusable share)
//! keeps the stopped VM and its configuration, reports a known failure, and
//! does not erase an earlier unresolved operation. Never starts a VM.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, MkdTemp, py_json};
use crate::support::hamn;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "start-preflight",
        "rejected start preserves stopped VM/configuration and reports known failure",
        vec![
            case("rejected_start_preserves_stopped_vm_and_configuration", rejected_start_preserves_stopped_vm_and_configuration),
            case("start_warns_when_the_host_volume_is_nearly_full", start_warns_when_the_host_volume_is_nearly_full),
            case("refused_share_changes_nothing_and_is_a_known_failure", refused_share_changes_nothing_and_is_a_known_failure),
            case("admission_refusal_reports_why_and_changes_nothing", admission_refusal_reports_why_and_changes_nothing),
            case("an_invalid_admission_limit_fails_before_any_change", an_invalid_admission_limit_fails_before_any_change),
            #[cfg(target_os = "macos")]
            case("a_running_vm_counts_until_its_vmrun_is_gone", a_running_vm_counts_until_its_vmrun_is_gone),
        ],
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

/// A start names a nearly full host volume before any VM work, and is not
/// refused because of it. The start used here is rejected afterwards for its
/// disk shrink, so no VM runs. `HAMN_TEST_HOST_FREE_MIB` stands in for the
/// measured free space.
fn start_warns_when_the_host_volume_is_nearly_full() {
    let temporary = MkdTemp::new("hamn-start-preflight-");
    let root = temporary.path();
    call(root, &["vm", "create", "--profile", "fixture", "--disk", "20", "--yes"], 0);
    let start = |free_mib: &str| {
        let mut command = Command::new(hamn());
        command
            .args(["--headless", "vm", "start", "--profile", "fixture", "--disk", "10", "--yes"])
            .env("HOME", root)
            .env("HAMN_TEST_HOST_FREE_MIB", free_mib);
        let result = api_fixtures::run(&mut command, None, Duration::from_secs(15));
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert!(result.json()["error"]["message"].as_str().is_some_and(|message| message.contains("disk size cannot shrink")), "{result:?}");
        result.stderr
    };
    // Below 10 GiB the start warns with the measured amount; at 10 GiB it does not.
    for (free_mib, warned) in [("0", true), ("1024", true), ("10239", true), ("10240", false), ("204800", false)] {
        let stderr = start(free_mib);
        let warning = format!("warning: {free_mib} MiB free on the volume holding ");
        assert_eq!(stderr.contains(&warning), warned, "{free_mib}: {stderr}");
        assert_eq!(stderr.contains("warning:"), warned, "{free_mib}: {stderr}");
    }
}

/// The names in `directory`.
fn entries(directory: &Path) -> BTreeSet<String> {
    fs::read_dir(directory).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect()
}

/// A stopped VM's start that refuses an unusable share does so before its
/// first change: no image, disk, seed or log directory is prepared and the
/// configuration stays, so the failure is known. An earlier unresolved
/// operation still stays unknown. Never starts a VM.
fn refused_share_changes_nothing_and_is_a_known_failure() {
    let temporary = MkdTemp::new("hamn-start-preflight-");
    let root = temporary.path();
    let outside = MkdTemp::new("hamn-start-share-");
    // `vm start` compares a mount location with its real path, and $TMPDIR
    // is behind a symbolic link on macOS.
    let outside = fs::canonicalize(outside.path()).unwrap();
    let home = fs::canonicalize(root).unwrap();
    call(root, &["vm", "create", "--profile", "fixture", "--disk", "20", "--yes"], 0);
    let profile = root.join(".hamn/fixture");
    let file = root.join("definition.yaml");
    let start = || {
        // A start that reached image preparation would run the updater; an
        // unreachable proxy keeps such a regression offline and fast.
        let mut command = Command::new(hamn());
        command
            .args(["--headless", "vm", "start", "--profile", "fixture", "--yes"])
            .env("HOME", root)
            .env("https_proxy", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9");
        let result = api_fixtures::run(&mut command, None, Duration::from_secs(15));
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        result.json()
    };
    for (location, writable, reason) in [
        (outside.clone(), true, "must be an owned non-symlink directory beneath HOME"),
        (home.join("missing"), false, "must be an owned non-symlink directory"),
    ] {
        let location = serde_json::to_string(location.to_str().unwrap()).unwrap();
        fs::write(
            &file,
            format!("apiVersion: hamn/v1\nkind: Profile\nmetadata:\n  name: fixture\nspec:\n  diskGiB: 20\n  mounts:\n    - location: {location}\n      mountPoint: /share\n      writable: {writable}\n"),
        )
        .unwrap();
        call(root, &["vm", "apply", "--profile", "fixture", "--file", file.to_str().unwrap(), "--yes"], 0);
        let config = fs::read(profile.join("config.yaml")).unwrap();
        let mut expected = entries(&profile);
        expected.insert("operation.json".into());
        let result = start();
        assert_eq!(result["error"]["code"], "operationFailed", "{result}");
        assert!(result["error"]["message"].as_str().is_some_and(|message| message.contains(reason)), "{result}");
        assert_eq!(entries(&profile), expected, "the refused start changed the profile directory");
        assert_eq!(fs::read(profile.join("config.yaml")).unwrap(), config);
        assert!(!root.join(".hamn/cache").exists(), "the refused start prepared a guest image");
        let state = call(root, &["vm", "status", "--profile", "fixture"], 0)["data"].clone();
        assert!(state["state"] == "stopped" && state["lastOperation"]["status"] == "failed", "{state}");
        assert_eq!(state["lastOperation"]["startedVm"], json!(false), "{state}");
    }
    // Refusing a share cannot resolve an earlier interrupted operation.
    let record = profile.join("operation.json");
    let mut value = api_fixtures::parse(&fs::read_to_string(&record).unwrap());
    value["status"] = json!("outcomeUnknown");
    fs::write(&record, py_json(&value)).unwrap();
    assert_eq!(start()["error"]["code"], "outcomeUnknown");
    let state = call(root, &["vm", "status", "--profile", "fixture"], 0)["data"].clone();
    assert_eq!(state["dockerStatus"], "recoveryRequired", "{state}");
}

/// `vm start --profile PROFILE` with `environment`, which must fail; its
/// JSON envelope. An admitted start would run the updater; an unreachable
/// proxy keeps such a regression offline and fast.
fn failed_start(root: &Path, profile: &str, arguments: &[&str], environment: &[(&str, &str)]) -> Value {
    let mut command = Command::new(hamn());
    command
        .args(["--headless", "vm", "start", "--profile", profile, "--yes"])
        .args(arguments)
        .env("HOME", root)
        .env("https_proxy", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9");
    for (key, value) in environment {
        command.env(key, value);
    }
    let result = api_fixtures::run(&mut command, None, Duration::from_secs(15));
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    result.json()
}

/// No VM fits in a reserve of all the memory there is.
const NO_ROOM: (&str, &str) = ("HAMN_ADMISSION_MEMORY_RESERVE_MIB", "4294967295");

/// `sysctl -n NAME` as a number, read apart from Hamn.
fn sysctl(name: &str) -> u64 {
    let output = Command::new("sysctl").args(["-n", name]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().parse().unwrap()
}

/// A start that admission refuses names the reason, what it measured, the
/// limits and the running VMs, and changes nothing: not the configuration
/// (not even the memory the start asked for), not the profile directory,
/// and no image is prepared.
fn admission_refusal_reports_why_and_changes_nothing() {
    let temporary = MkdTemp::new("hamn-start-preflight-");
    let root = temporary.path();
    call(root, &["vm", "create", "--profile", "fixture", "--yes"], 0);
    let profile = root.join(".hamn/fixture");
    let config = fs::read(profile.join("config.yaml")).unwrap();
    let mut expected = entries(&profile);
    expected.insert("operation.json".into());

    let result = failed_start(root, "fixture", &["--memory", "2"], &[NO_ROOM]);
    let error = &result["error"];
    assert_eq!(error["code"], "admissionRefused", "{result}");
    let message = error["message"].as_str().unwrap();
    for part in [
        "refused to start the VM of profile fixture: ",
        "this VM's 2048 MiB exceed the ",
        "less the 4294967295 MiB reserve (HAMN_ADMISSION_MEMORY_RESERVE_MIB)",
        "No other Hamn VM runs. Nothing was started",
    ] {
        assert!(message.contains(part), "{part:?} missing from {message}");
    }
    let details = &error["details"];
    assert_eq!(details["admission"], "refused", "{details}");
    assert_eq!(details["profile"], "fixture", "{details}");
    assert_eq!(details["request"], json!({"cpus":4, "memoryMiB":2048}), "{details}");
    assert!(details["reasons"].as_array().unwrap().contains(&json!("memory")), "{details}");
    assert_eq!(details["unread"], json!([]), "{details}");
    assert_eq!(details["runningProfiles"], json!([]), "{details}");
    assert_eq!(
        details["limits"],
        json!({"memoryReserveMiB":4294967295u64, "memoryPressure":"critical", "maxLoadPerCpu":null, "minBatteryPercent":0}),
        "{details}"
    );
    let measured = &details["measured"];
    assert_eq!(measured["totalMemoryMiB"], sysctl("hw.memsize") / (1024 * 1024), "{measured}");
    assert_eq!(measured["logicalCpus"], sysctl("hw.logicalcpu"), "{measured}");
    assert_eq!(measured["runningMemoryMiB"], 0, "{measured}");
    assert!(measured["loadAverage1m"].as_f64().is_some_and(|load| load >= 0.0), "{measured}");
    assert!(["normal", "warn", "critical"].contains(&measured["memoryPressure"].as_str().unwrap()), "{measured}");
    assert!(["ac", "battery", "ups"].contains(&measured["powerSource"].as_str().unwrap()), "{measured}");

    assert_eq!(entries(&profile), expected, "the refused start changed the profile directory");
    assert_eq!(fs::read(profile.join("config.yaml")).unwrap(), config);
    assert!(!root.join(".hamn/cache").exists(), "the refused start prepared a guest image");
    let state = call(root, &["vm", "status", "--profile", "fixture"], 0)["data"].clone();
    assert!(state["state"] == "stopped" && state["lastOperation"]["status"] == "failed", "{state}");
    assert_eq!(state["lastOperation"]["startedVm"], json!(false), "{state}");
    assert_eq!(state["lastOperation"]["error"], json!(message), "{state}");
}

fn an_invalid_admission_limit_fails_before_any_change() {
    let temporary = MkdTemp::new("hamn-start-preflight-");
    let root = temporary.path();
    call(root, &["vm", "create", "--profile", "fixture", "--yes"], 0);
    let profile = root.join(".hamn/fixture");
    let config = fs::read(profile.join("config.yaml")).unwrap();
    let mut expected = entries(&profile);
    expected.insert("operation.json".into());
    let result = failed_start(root, "fixture", &[], &[("HAMN_ADMISSION_MIN_BATTERY_PERCENT", "101")]);
    assert_eq!(result["error"]["code"], "operationFailed", "{result}");
    assert_eq!(
        result["error"]["message"],
        "HAMN_ADMISSION_MIN_BATTERY_PERCENT must be a decimal number from 0 to 100",
        "{result}"
    );
    assert!(result["error"].get("details").is_none(), "{result}");
    assert_eq!(entries(&profile), expected);
    assert_eq!(fs::read(profile.join("config.yaml")).unwrap(), config);
}

/// A process standing in for a profile's vmrun, killed when dropped.
#[cfg(target_os = "macos")]
struct StandIn(std::process::Child);

#[cfg(target_os = "macos")]
impl Drop for StandIn {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A profile whose vmrun the probe verifies counts with its configured
/// memory, the refusal leaves that process running, and once the process
/// is gone (a crashed supervisor) the next start no longer counts it.
#[cfg(target_os = "macos")]
fn a_running_vm_counts_until_its_vmrun_is_gone() {
    use crate::suites::workspace_live::processes::{System, Table};
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = MkdTemp::new("hamn-start-preflight-");
    let root = temporary.path();
    call(root, &["vm", "create", "--profile", "busy", "--memory", "3", "--yes"], 0);
    call(root, &["vm", "create", "--profile", "work", "--yes"], 0);
    let mut stand_in = StandIn(Command::new("/bin/sleep").arg("600").spawn().unwrap());
    let identity = System.identity(stand_in.0.id() as i32).unwrap();
    let busy = root.join(".hamn/busy");
    for (file, text) in [
        (
            "vmrun.identity",
            format!("{} {} {} {}\n", identity.pid, identity.start_sec, identity.start_usec, identity.executable_uuid),
        ),
        ("vmrun.pid", format!("{}\n", identity.pid)),
    ] {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        std::io::Write::write_all(&mut options.open(busy.join(file)).unwrap(), text.as_bytes()).unwrap();
    }

    let result = failed_start(root, "work", &[], &[NO_ROOM]);
    let details = &result["error"]["details"];
    assert_eq!(details["runningProfiles"], json!([{"name":"busy", "memoryMiB":3072, "verified":true}]), "{result}");
    assert_eq!(details["measured"]["runningMemoryMiB"], 3072, "{result}");
    assert!(result["error"]["message"].as_str().unwrap().contains(". Running VMs: busy 3072 MiB. "), "{result}");
    assert!(stand_in.0.try_wait().unwrap().is_none(), "the refusal ended another profile's vmrun");

    stand_in.0.kill().unwrap();
    stand_in.0.wait().unwrap();
    let result = failed_start(root, "work", &[], &[NO_ROOM]);
    let details = &result["error"]["details"];
    assert_eq!(details["runningProfiles"], json!([]), "{result}");
    assert_eq!(details["measured"]["runningMemoryMiB"], 0, "{result}");
}
