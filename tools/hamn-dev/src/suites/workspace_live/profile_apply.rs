//! Declarative settings cross the real worker, VM and guest boundaries.
//! Only the workspace-owned `verify` profile is changed. Its original
//! configuration and permissions are restored even after a failed assertion,
//! before the outer harness attempts VM cleanup. No disk is removed.
use super::{Live, PROFILE, Spec, finally, path_str, process};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn definition(cpus: u32, memory: u32, share: &str) -> String {
    // YAML quoting must also work when the temporary root contains spaces.
    let share = serde_json::to_string(share).unwrap();
    format!(
        "apiVersion: hamn/v1\nkind: Profile\nmetadata:\n  name: verify\nspec:\n  cpus: {cpus}\n  memoryMiB: {memory}\n  diskGiB: 60\n  mountHome: false\n  mounts:\n    - location: {share}\n      mountPoint: /hamn-apply-proof\n      writable: true\n"
    )
}

fn refused(live: &Live, words: &[&str], flags: &[&str], code: &str) -> Value {
    let args = [&["--headless"][..], words, &["--profile", PROFILE], flags].concat();
    let output = process::capture(
        live.runtime.binary.as_os_str(),
        &args,
        &Spec { environment: Some(&live.runtime.environment), input: None },
        live.runtime.timeout,
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.code(), Some(1), "{output:?}");
    assert_eq!(value["schemaVersion"], 1, "{value}");
    assert_eq!(value["ok"], false, "{value}");
    assert_eq!(value["error"]["code"], code, "{value}");
    assert!(value["data"].is_null(), "{value}");
    value
}

fn guest(live: &Live, cpus: u32, memory: u64) -> Value {
    // Independent observations: no host status/config value supplies the
    // expected CPU count or proves that virtiofs reached the guest.
    let output = live.ssh(
        "getconf _NPROCESSORS_ONLN; awk '/^MemTotal:/ {print $2}' /proc/meminfo; cat /hamn-apply-proof/host-proof; printf guest-proof > /hamn-apply-proof/guest-proof",
    );
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 3, "{output}");
    assert_eq!(lines[0].parse::<u32>().unwrap(), cpus, "{output}");
    let kib = lines[1].parse::<u64>().unwrap();
    // Linux reserves part of RAM. Accept <=10% reservation, never host
    // persistence as evidence of guest RAM changing.
    assert!(kib >= memory * 1024 * 9 / 10 && kib <= memory * 1024, "{output}");
    assert_eq!(lines[2], "host-proof");
    assert_eq!(fs::read(live.root.join("apply-share/guest-proof")).unwrap(), b"guest-proof");
    json!({"onlineCpus": cpus, "memTotalKiB": kib, "writableMount": true})
}

pub(super) fn verify(live: &Live) {
    live.assert_owned();
    assert_ne!(unsafe { libc::geteuid() }, 0, "live permission check requires an unprivileged user");
    let config = live.profile().join("config.yaml");
    let original = fs::read(&config).unwrap();
    let permissions = fs::metadata(&config).unwrap().permissions();
    let share = live.root.join("apply-share");
    fs::create_dir_all(&share).unwrap();
    fs::write(share.join("host-proof"), b"host-proof\n").unwrap();
    let file = live.root.join("apply-definition.yaml");
    let flags = ["--file", path_str(&file), "--yes"];
    let mut evidence = Vec::new();
    finally(
        &mut evidence,
        |evidence| {
            live.call(&["vm", "stop"], &["--yes"]);
            fs::write(&file, definition(2, 4096, path_str(&share))).unwrap();
            assert_eq!(live.call(&["vm", "apply"], &flags)["action"], "configure");
            let stored = fs::read(&config).unwrap();
            live.call(&["vm", "start"], &["--yes"]);
            evidence.push(guest(live, 2, 4096));
            let pid = live.vm_pid();
            assert_eq!(live.call(&["vm", "apply"], &flags)["action"], "none");
            assert_eq!(fs::read(&config).unwrap(), stored);
            assert_eq!(live.vm_pid(), pid);

            fs::write(&file, definition(3, 5120, path_str(&share))).unwrap();
            evidence.push(refused(live, &["vm", "apply"], &flags, "conflict"));
            assert_eq!(fs::read(&config).unwrap(), stored);
            assert_eq!(live.vm_pid(), pid);
            guest(live, 2, 4096);
            // A dry run is a plan, including while running; it does not
            // promise that a mutation is currently executable.
            assert_eq!(live.call(&["vm", "apply"], &["--file", path_str(&file), "--dry-run"])["action"], "configure");
            assert_eq!(fs::read(&config).unwrap(), stored);
            live.call(&["vm", "stop"], &["--yes"]);
            assert_eq!(live.call(&["vm", "apply"], &flags)["action"], "configure");
            let repaired = fs::read(&config).unwrap();
            live.call(&["vm", "start"], &["--yes"]);
            evidence.push(guest(live, 3, 5120));
            // Corrupt configuration does not stop the independently owned VM.
            // Operations refuse it until an explicit repair restores readability.
            let running_pid = live.vm_pid();
            fs::write(&config, b"unknown: true\n").unwrap();
            evidence.push(refused(live, &["vm", "status"], &[], "profileUnavailable"));
            evidence.push(refused(live, &["vm", "stop"], &["--yes"], "operationFailed"));
            evidence.push(refused(live, &["vm", "apply"], &flags, "operationFailed"));
            assert_eq!(fs::read(&config).unwrap(), b"unknown: true\n");
            assert_eq!(live.vm_pid(), running_pid);
            assert_eq!(live.docker(&["version", "--format", "{{.Server.Os}}"]).trim(), "linux");
            fs::write(&config, &repaired).unwrap();
            assert_eq!(live.call(&["vm", "status"], &[])["state"], "running");
            assert_eq!(live.vm_pid(), running_pid);
            live.call(&["vm", "stop"], &["--yes"]);

            fs::write(&config, b"cpus: [\n").unwrap();
            evidence.push(refused(live, &["vm", "apply"], &flags, "operationFailed"));
            assert_eq!(fs::read(&config).unwrap(), b"cpus: [\n");
            fs::write(&config, &repaired).unwrap();
            fs::set_permissions(&config, fs::Permissions::from_mode(0)).unwrap();
            evidence.push(refused(live, &["vm", "apply"], &flags, "operationFailed"));
            fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(fs::read(&config).unwrap(), repaired);
            assert_eq!(live.call(&["vm", "apply"], &flags)["action"], "none");
            live.call(&["vm", "start"], &["--yes"]);
            evidence.push(guest(live, 3, 5120));
            live.write_json("vm-apply-results.json", &json!({"phase": "assertions-complete", "observations": evidence}));
        },
        |_| {
            // Restore readability first so the owned stop can read the
            // profile, including after a corruption/permission assertion.
            fs::set_permissions(&config, permissions.clone()).unwrap();
            fs::write(&config, &original).unwrap();
            live.runtime.stop(&[PROFILE.to_owned()]).unwrap();
        },
    );
    // The remaining live checks expect the baseline VM to be running.
    live.call(&["vm", "start"], &["--yes"]);
    live.write_json("vm-apply-results.json", &json!({"passed": true, "observations": evidence}));
    println!("PASS: live vm apply idempotency, running conflict, guest CPU/RAM/mount and explicit configuration recovery");
}
