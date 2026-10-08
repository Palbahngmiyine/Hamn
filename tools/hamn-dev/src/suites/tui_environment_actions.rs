//! Environment rows cannot reuse VM operations from a previous panel: a
//! Hamn profile and a Docker context sharing a name never dispatch VM
//! actions from the environment picker, and VM configure is offered only in
//! a real Hamn VM panel. A profile whose configuration cannot be read is
//! listed with the reason, beside the profiles that read, and is offered no
//! command made of settings.
use crate::runner::{self, Case, case};
use crate::support::tui::Harness;
use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = ["docker", "hamn"]
        .into_iter()
        .map(|kind| case(format!("check_environment_actions/{kind}"), move || check_environment_actions(kind)))
        .collect();
    cases.push(case("an_unreadable_profile_is_listed_with_its_reason", an_unreadable_profile_is_listed_with_its_reason));
    runner::run(
        "tui-environment-actions",
        "colliding Docker/Hamn names cannot dispatch VM actions in the environment picker; VM configure remains available only in a real Hamn VM panel; an unreadable profile is listed with its reason",
        cases,
        filters,
    )
}

fn close_input(harness: &mut Harness, marker: &str) {
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(marker));
}

/// One profile whose `config.yaml` cannot be read, here with the removed
/// `kubernetes` mapping, beside one that reads. The VM panel and the
/// environment picker list both, the unreadable one with the state of its VM
/// and the reason; `c` shows where its file is instead of a `vm configure`
/// command. No VM is started and nothing is confirmed.
fn an_unreadable_profile_is_listed_with_its_reason() {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    for (name, text) in [("external", "cpus: 2\nmemoryMiB: 2048\ndiskGiB: 60\n"), ("broken", "kubernetes:\n  enabled: true\n")] {
        let profile = harness.root.join(".hamn").join(name);
        fs::DirBuilder::new().mode(0o700).create(&profile).unwrap();
        let config = profile.join("config.yaml");
        fs::write(&config, text).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let broken = harness.root.join(".hamn/broken/config.yaml");
    let original = fs::read(&broken).unwrap();
    File::create(harness.root.join("released")).unwrap();
    let reason = "unreadable configuration: unknown configuration key: kubernetes";

    harness.send(b"v", "vm list");
    harness.until(&format!("broken  stopped  {reason}"));
    harness.until("external  stopped");
    // The filter leaves the unreadable profile as the selected row.
    harness.write(b"/broken\r");
    harness.wait(|harness| !harness.text().contains("external  stopped"));
    harness.send(b"c", "Configuration: cannot be read: unknown configuration key: kubernetes");
    let text = harness.text();
    assert!(text.contains(".hamn/broken/config.yaml"), "{text}");
    assert!(!text.contains("vm configure"), "{text}");
    // Esc closes the detail and the VM panel.
    harness.send(b"\x1b", "old-target-row");
    harness.send(b"e", "Container environments");
    harness.until(&format!("broken  Hamn profile  stopped  {reason}"));
    harness.until("external  Hamn profile  stopped");

    assert_eq!(fs::read(&broken).unwrap(), original);
    let calls = harness.calls();
    let mutated = calls.iter().any(|(_, args)| args.iter().any(|arg| arg == "start" || arg == "stop" || arg == "rm"));
    assert!(!mutated, "{calls:?}");
}

fn check_environment_actions(kind: &str) {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    // The owned, stopped profile and recorded Docker context deliberately
    // share a name. No VM is started and no mutation is ever confirmed.
    let profile = harness.root.join(".hamn/external");
    fs::DirBuilder::new().mode(0o700).create(&profile).unwrap();
    let config = profile.join("config.yaml");
    fs::write(&config, "cpus: 2\nmemoryMiB: 2048\ndiskGiB: 60\n").unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let original = fs::read(&config).unwrap();
    File::create(harness.root.join("released")).unwrap();
    harness.send(b"v", "vm list");
    harness.until("external");
    harness.send(b"e", "Container environments");
    harness.wait(|harness| !harness.text().contains("[loading]"));
    harness.until("external");
    if kind == "docker" {
        harness.send(b"j:ROW_BARRIER", ":ROW_BARRIER");
        close_input(&mut harness, ":ROW_BARRIER");
    }
    for key in *b"tsdrgl" {
        let marker = ":ACTION_BARRIER";
        harness.write(&[&[key][..], marker.as_bytes()].concat());
        harness.wait(|harness| harness.text().contains(marker) || harness.text().contains("Confirm vm"));
        assert!(!harness.text().contains("Confirm vm"), "{}", harness.text());
        assert!(harness.text().contains("Container environments"), "{}", harness.text());
        close_input(&mut harness, marker);
        harness.until("Select an environment with Enter");
    }
    for key in *b"cv" {
        harness.send(&[&[key][..], b":PANEL_BARRIER"].concat(), ":PANEL_BARRIER");
        assert!(!harness.text().contains("vm configure"), "{}", harness.text());
        assert!(harness.text().contains("Container environments"), "{}", harness.text());
        close_input(&mut harness, ":PANEL_BARRIER");
    }
    let text = harness.text();
    assert!(text.contains("external  Hamn profile"), "{text}");
    assert!(text.contains("external  Docker context"), "{text}");
    assert!(text.contains("unix:///external/docker.sock"), "{text}");
    assert!(!text.contains("t stop"), "{text}");
    assert!(!text.contains("v VM settings"), "{text}");
    harness.send(b"\r", if kind == "docker" { "new-target-row" } else { "old-target-row" });
    harness.wait(|harness| !harness.text().contains("[loading]"));
    if kind == "docker" {
        assert!(harness.text().contains("--context external"), "{}", harness.text());
        // request.words still contains vm list, but native external browsing
        // must not enable the configure shortcut either.
        harness.send(b"c:EXTERNAL_BARRIER", ":EXTERNAL_BARRIER");
        assert!(!harness.text().contains("vm configure"), "{}", harness.text());
        assert!(!harness.text().contains("v VM settings"), "{}", harness.text());
        close_input(&mut harness, ":EXTERNAL_BARRIER");
    } else {
        harness.send(b"v", "vm list");
        harness.until("external");
        harness.wait(|harness| !harness.text().contains("[loading]"));
        harness.send(b"c", "vm configure --profile external --cpu 2 --memory 2 --disk 60");
        close_input(&mut harness, "vm configure");
    }
    assert_eq!(fs::read(&config).unwrap(), original);
    let mut names: Vec<String> = fs::read_dir(&profile)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["config.yaml"]);
    let calls = harness.calls();
    let mutated = calls.iter().any(|(_, args)| args.iter().any(|arg| arg == "start" || arg == "stop" || arg == "rm"));
    assert!(!mutated, "{calls:?}");
}
