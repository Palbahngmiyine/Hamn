//! The physical release gate's deterministic contracts: candidate and
//! evidence validation, archive safety, the isolated runtime's command and
//! environment boundaries, terminal restoration on a real PTY and the
//! harness's input handling. Real VM checks run only in `make release-gate`.
use crate::release::archive::{Limits, unpack, unpack_with};
use crate::release::contract::{CHECKS, physical_evidence, validate_candidate, validate_physical};
use crate::release::files::{canonical_json, sha256_file};
use crate::release::kubernetes;
use crate::release::physical::{Config, workspace};
use crate::release::runtime::Runtime;
use crate::runner::{self, case};
use crate::support::tmp::TempDir;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;
use tar::{EntryType, Header};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-physical",
        "physical gate input, archive, provenance and runtime contracts",
        vec![
            case(
                "candidate_hashes_exact_file_set_and_source_identity",
                candidate_hashes_exact_file_set_and_source_identity,
            ),
            case(
                "missing_false_foreign_and_legacy_evidence_is_rejected",
                missing_false_foreign_and_legacy_evidence_is_rejected,
            ),
            case("harness_evidence_satisfies_the_contract", harness_evidence_satisfies_the_contract),
            case(
                "candidate_archive_rejects_links_duplicates_and_traversal",
                candidate_archive_rejects_links_duplicates_and_traversal,
            ),
            case(
                "candidate_archive_limits_ownership_and_single_root",
                candidate_archive_limits_ownership_and_single_root,
            ),
            case(
                "workspace_keeps_profile_sockets_below_darwin_path_limit",
                workspace_keeps_profile_sockets_below_darwin_path_limit,
            ),
            case(
                "headless_operations_target_only_the_isolated_profile",
                headless_operations_target_only_the_isolated_profile,
            ),
            case(
                "log_records_are_read_as_ndjson_and_failures_are_not_hidden",
                log_records_are_read_as_ndjson_and_failures_are_not_hidden,
            ),
            case(
                "stop_reports_every_profile_it_cannot_prove_stopped",
                stop_reports_every_profile_it_cannot_prove_stopped,
            ),
            case(
                "terminal_quit_drains_output_larger_than_the_pty_queue",
                terminal_quit_drains_output_larger_than_the_pty_queue,
            ),
            case(
                "terminal_requires_render_zero_exit_and_restored_settings",
                terminal_requires_render_zero_exit_and_restored_settings,
            ),
            case(
                "invalid_network_mode_is_rejected_before_any_runtime_action",
                invalid_network_mode_is_rejected_before_any_runtime_action,
            ),
            case("help_needs_no_validator_environment_or_runtime", help_needs_no_validator_environment_or_runtime),
            case("kubernetes_harness_arguments_and_existing_output", kubernetes_harness_arguments_and_existing_output),
        ],
        filters,
    )
}

const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn sha(path: &Path) -> String {
    sha256_file(path).unwrap()
}

fn candidate_fixture(root: &Path) -> String {
    let names = [
        "hamn-v1.0.0-darwin-arm64.tar.gz",
        "hamn-v1.0.0-ubuntu-24.04-arm64.img",
        "hamn-v1.0.0.spdx.json",
        "install.sh",
    ];
    for name in names {
        fs::write(root.join(name), "fixture").unwrap();
    }
    let artifacts: Vec<Value> =
        names.iter().map(|name| json!({"name": name, "sha256": sha(&root.join(name))})).collect();
    let candidate = json!({"schemaVersion": 1, "kind": "hamn-release-candidate", "tag": "v1.0.0-rc.1", "version": "v1.0.0",
        "commit": HEX_A, "sourceTree": HEX_B, "artifacts": artifacts});
    fs::write(root.join("candidate.json"), candidate.to_string()).unwrap();
    let checksums: String =
        names.iter().chain(&["candidate.json"]).map(|name| format!("{}  {name}\n", sha(&root.join(name)))).collect();
    fs::write(root.join("SHA256SUMS"), &checksums).unwrap();
    checksums
}

fn candidate_hashes_exact_file_set_and_source_identity() {
    let directory = TempDir::new("hamn-release-candidate-");
    let root = directory.path();
    let checksums = candidate_fixture(root);
    let validate = |tag: &str, commit: &str, tree: &str| validate_candidate(root, tag, commit, tree);
    validate("v1.0.0-rc.1", HEX_A, HEX_B).unwrap();
    assert!(validate("v1.0.0-rc.2", HEX_A, HEX_B).is_err());
    assert!(validate("v1.0.0-rc.1", HEX_B, HEX_B).is_err());
    assert!(validate("v1.0.0-rc.1", HEX_A, HEX_A).is_err());
    assert!(validate("v1.0.0", HEX_A, HEX_B).is_err());
    fs::write(root.join("extra"), "unbound").unwrap();
    assert!(validate("v1.0.0-rc.1", HEX_A, HEX_B).unwrap_err().contains("artifact set"));
    fs::remove_file(root.join("extra")).unwrap();
    fs::write(root.join("install.sh"), "tampered").unwrap();
    assert!(validate("v1.0.0-rc.1", HEX_A, HEX_B).unwrap_err().contains("digest mismatch"));
    fs::write(root.join("install.sh"), "fixture").unwrap();
    fs::write(root.join("SHA256SUMS"), format!("{checksums}{}  ../outside\n", "0".repeat(64))).unwrap();
    assert!(validate("v1.0.0-rc.1", HEX_A, HEX_B).is_err());
    fs::write(root.join("SHA256SUMS"), checksums.lines().skip(1).map(|line| format!("{line}\n")).collect::<String>())
        .unwrap();
    assert!(validate("v1.0.0-rc.1", HEX_A, HEX_B).unwrap_err().contains("binding mismatch"));
    fs::write(root.join("SHA256SUMS"), &checksums).unwrap();
    // A link to identical bytes is still not the candidate's own file.
    fs::rename(root.join("install.sh"), root.join("../install.sh.real")).ok();
    let real = root.parent().unwrap().join("install.sh.real");
    std::os::unix::fs::symlink(&real, root.join("install.sh")).unwrap();
    assert!(validate("v1.0.0-rc.1", HEX_A, HEX_B).is_err());
    fs::remove_file(root.join("install.sh")).unwrap();
    fs::rename(&real, root.join("install.sh")).unwrap();
    validate("v1.0.0-rc.1", HEX_A, HEX_B).unwrap();
}

struct EvidenceFixture {
    _directory: TempDir,
    root: PathBuf,
    evidence: Value,
}

impl EvidenceFixture {
    fn new() -> Self {
        let directory = TempDir::new("hamn-release-evidence-");
        let root = directory.path().to_path_buf();
        let candidate = json!({"tag": "v1.0.0-rc.1", "commit": HEX_A, "sourceTree": HEX_B, "artifacts": [{"name": "host.tar.gz", "sha256": "c".repeat(64)}]});
        fs::write(root.join("candidate"), candidate.to_string()).unwrap();
        fs::write(root.join("checksums"), "fixture checksums").unwrap();
        let kubernetes = json!({"kind": "hamn-external-kubernetes-e2e", "passed": true, "namespaceRemoved": true, "kubeconfigUnchanged": true});
        let evidence = physical_evidence(
            &candidate,
            &sha(&root.join("candidate")),
            &sha(&root.join("checksums")),
            "1",
            "2",
            kubernetes,
        )
        .unwrap();
        Self { _directory: directory, root, evidence }
    }

    fn verify(&self, value: &Value) -> Result<Value, String> {
        fs::write(self.root.join("evidence"), value.to_string()).unwrap();
        validate_physical(
            &self.root.join("candidate"),
            &self.root.join("checksums"),
            &self.root.join("evidence"),
            "1",
            "2",
        )
    }

    fn rejects(&self, change: impl FnOnce(&mut Value)) -> String {
        let mut value = self.evidence.clone();
        change(&mut value);
        self.verify(&value).expect_err(&format!("accepted {value}"))
    }
}

fn missing_false_foreign_and_legacy_evidence_is_rejected() {
    let fixture = EvidenceFixture::new();
    fixture.verify(&fixture.evidence).unwrap();
    for check in CHECKS {
        fixture.rejects(|value| value["checks"][check] = json!(false));
        fixture.rejects(|value| value["checks"][check] = json!("true"));
        fixture.rejects(|value| drop(value["checks"].as_object_mut().unwrap().remove(check)));
    }
    for (section, key, replacement) in [
        ("workflow", "run", json!("3")),
        ("workflow", "attempt", json!("1")),
        ("candidate", "candidateJsonSha256", json!("0".repeat(64))),
        ("candidate", "checksumsSha256", json!("0".repeat(64))),
        ("kubernetes", "namespaceRemoved", json!(false)),
        ("kubernetes", "passed", json!(false)),
        ("kubernetes", "kubeconfigUnchanged", json!(false)),
        ("kubernetes", "kind", json!("other")),
    ] {
        fixture.rejects(|value| value[section][key] = replacement);
    }
    fixture.rejects(|value| value["candidate"]["artifacts"]["host.tar.gz"] = json!("d".repeat(64)));
    fixture.rejects(|value| value["tag"] = json!("v1.0.0-rc.2"));
    fixture.rejects(|value| value["validationMode"] = json!("github-hosted-no-vm"));
    // Schema 2 evidence, with its retirement checks and legacy section, no
    // longer counts.
    assert!(fixture.rejects(|value| value["schemaVersion"] = json!(2)).contains("identity"));
    assert!(fixture.rejects(|value| value["legacy"] = json!({"running": {}, "stopped": {}})).contains("schema"));
    for removed in ["k3sRunningRetirement", "k3sStoppedRetirement", "dockerDataPreserved"] {
        assert!(fixture.rejects(|value| value["checks"][removed] = json!(true)).contains("checks"));
    }
    fixture.rejects(|value| drop(value.as_object_mut().unwrap().remove("kubernetes")));
}

fn harness_evidence_satisfies_the_contract() {
    let fixture = EvidenceFixture::new();
    let evidence = &fixture.evidence;
    assert_eq!(evidence["schemaVersion"], json!(3));
    assert!(evidence.get("legacy").is_none());
    assert_eq!(evidence["checks"].as_object().unwrap().len(), CHECKS.len());
    assert!(
        !CHECKS
            .iter()
            .any(|check| check.contains("k3s") || check.contains("Retirement") || *check == "dockerDataPreserved")
    );
    // The written form is canonical: sorted keys, compact, one newline.
    let text = canonical_json(evidence);
    assert!(text.starts_with("{\"candidate\":{\"artifacts\":") && text.ends_with("}\n"), "{text}");
    fixture.verify(&serde_json::from_str(&text).unwrap()).unwrap();
}

/// Appends a member with a raw header, so hostile names reach the archive.
fn raw_member(builder: &mut tar::Builder<File>, name: &str, kind: EntryType, data: &[u8]) {
    let mut header = Header::new_gnu();
    header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
    header.set_entry_type(kind);
    header.set_size(data.len() as u64);
    header.set_mode(0o755);
    if kind == EntryType::Symlink {
        header.as_gnu_mut().unwrap().linkname[..8].copy_from_slice(b"/outside");
    }
    header.set_cksum();
    builder.append(&header, data).unwrap();
}

fn archive(path: &Path, members: &[(&str, EntryType)]) {
    let mut builder = tar::Builder::new(File::create(path).unwrap());
    for (name, kind) in members {
        let data: &[u8] = if *kind == EntryType::Regular { b"x" } else { b"" };
        raw_member(&mut builder, name, *kind, data);
    }
    builder.finish().unwrap();
}

fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn candidate_archive_rejects_links_duplicates_and_traversal() {
    use EntryType::{Directory, Link, Regular, Symlink};
    let cases: &[(&[(&str, EntryType)], bool)] = &[
        (&[("root/bin/hamn", Regular)], true),
        (&[("root/", Directory), ("root/bin/", Directory), ("root/bin/hamn", Regular)], true),
        (&[("../outside", Regular)], false),
        (&[("root/../../outside", Regular)], false),
        (&[("/outside", Regular)], false),
        (&[("root/a", Regular), ("root/a", Regular)], false),
        (&[("root/a", Regular), ("root/./a", Regular)], false),
        (&[("root/link", Symlink)], false),
        (&[("root/hard", Link)], false),
        (&[("root/fifo", EntryType::Fifo)], false),
        (&[("root/ok", Regular), ("root/link", Symlink)], false),
    ];
    for (members, valid) in cases {
        let directory = TempDir::new("hamn-release-archive-");
        let path = directory.path().join("archive.tar");
        archive(&path, members);
        let destination = directory.path().join("unpacked");
        fs::create_dir(&destination).unwrap();
        let result = unpack(&path, &destination);
        if *valid {
            assert_eq!(
                result.map_err(|error| format!("{members:?}: {error}: {:?}", listing(&destination))).unwrap(),
                destination.join("root")
            );
            assert_eq!(fs::read(destination.join("root/bin/hamn")).unwrap(), b"x");
            // Extracted modes drop group/other write and special bits.
            assert_eq!(fs::metadata(destination.join("root/bin/hamn")).unwrap().permissions().mode() & 0o7777, 0o755);
        } else {
            assert!(result.is_err(), "accepted {members:?}");
            assert!(listing(&destination).is_empty(), "{members:?} wrote {:?}", listing(&destination));
        }
    }
    // The real candidate format: a gzip-compressed archive with one root.
    let directory = TempDir::new("hamn-release-archive-");
    let source = directory.path().join("hamn-v1.0.0-darwin-arm64");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::write(source.join("bin/hamn"), b"executable").unwrap();
    let path = directory.path().join("host.tar.gz");
    // As `release build-candidate` does: without COPYFILE_DISABLE, macOS tar adds
    // `._*` AppleDouble members, which make a second root.
    let status = Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .arg("-C")
        .arg(directory.path())
        .arg("-czf")
        .arg(&path)
        .arg("hamn-v1.0.0-darwin-arm64")
        .status()
        .unwrap();
    assert!(status.success());
    let destination = directory.path().join("unpacked");
    fs::create_dir(&destination).unwrap();
    let root = unpack(&path, &destination).unwrap();
    assert_eq!(fs::read(root.join("bin/hamn")).unwrap(), b"executable");
}

fn candidate_archive_limits_ownership_and_single_root() {
    let directory = TempDir::new("hamn-release-archive-");
    let path = directory.path().join("archive.tar");
    archive(&path, &[("root/a", EntryType::Regular), ("root/b", EntryType::Regular), ("root/c", EntryType::Regular)]);
    let unpacked = |name: &str, limits: &Limits| {
        let destination = directory.path().join(name);
        fs::create_dir(&destination).unwrap();
        (unpack_with(&path, &destination, limits), destination)
    };
    assert!(unpacked("fits", &Limits { entries: 3, bytes: 3 }).0.is_ok());
    for (name, limits) in [("entries", Limits { entries: 2, bytes: 100 }), ("bytes", Limits { entries: 100, bytes: 2 })]
    {
        let (result, destination) = unpacked(name, &limits);
        assert!(result.unwrap_err().contains("excessive"), "{name}");
        assert!(listing(&destination).is_empty());
    }
    let empty = directory.path().join("empty.tar");
    tar::Builder::new(File::create(&empty).unwrap()).finish().unwrap();
    let destination = directory.path().join("empty");
    fs::create_dir(&destination).unwrap();
    assert!(unpack(&empty, &destination).unwrap_err().contains("excessive"));
    let two_roots = directory.path().join("two-roots.tar");
    archive(&two_roots, &[("one/a", EntryType::Regular), ("two/b", EntryType::Regular)]);
    let destination = directory.path().join("two");
    fs::create_dir(&destination).unwrap();
    assert!(unpack(&two_roots, &destination).unwrap_err().contains("exactly one archive root"));
    // An archive another path can change is not a validation input.
    fs::hard_link(&path, directory.path().join("second-name.tar")).unwrap();
    let destination = directory.path().join("linked");
    fs::create_dir(&destination).unwrap();
    assert!(unpack(&path, &destination).unwrap_err().contains("unsafe"));
    assert!(listing(&destination).is_empty());
}

fn workspace_keeps_profile_sockets_below_darwin_path_limit() {
    let work = workspace().unwrap();
    let mode = fs::metadata(&work).unwrap().permissions().mode() & 0o777;
    fs::remove_dir(&work).unwrap();
    assert!(
        work.starts_with("/private/tmp") && work.file_name().unwrap().to_string_lossy().starts_with("hamn-e2e-"),
        "{work:?}"
    );
    // The longest socket name in the longest harness profile.
    let socket = work.join("home/.hamn/default/cleanup.sock");
    assert!(socket.as_os_str().len() < 104, "{socket:?}");
    assert_eq!(mode, 0o700);
}

/// A fake Hamn: records its arguments and environment, prints `response`.
fn recording_runtime(directory: &Path, response: &str) -> Runtime {
    let binary = directory.join("hamn");
    fs::write(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >\"$HOME/args\"\n/usr/bin/env >\"$HOME/env\"\ncat \"$HOME/response\"\n",
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(directory.join("response"), response).unwrap();
    Runtime::new(binary, directory, "/usr/local/bin/docker")
}

fn headless_operations_target_only_the_isolated_profile() {
    let directory = TempDir::new("hamn-release-runtime-");
    let runtime = recording_runtime(
        directory.path(),
        &json!({"schemaVersion": 1, "ok": true, "data": {"state": "stopped"}}).to_string(),
    );
    assert_eq!(runtime.call(&["vm", "stop"], "second", &["--yes"]).unwrap(), json!({"state": "stopped"}));
    let args = fs::read_to_string(directory.path().join("args")).unwrap();
    assert_eq!(args.lines().collect::<Vec<_>>(), ["--headless", "vm", "stop", "--profile", "second", "--yes"]);
    let environment: BTreeMap<String, String> = fs::read_to_string(directory.path().join("env"))
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('=').map(|(key, value)| (key.to_owned(), value.to_owned())))
        .collect();
    assert_eq!(environment["HOME"], directory.path().to_string_lossy());
    assert_eq!(environment["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin");
    assert_eq!(environment["LC_ALL"], "C");
    // Only what the shell itself adds may appear besides the exact set.
    let unexpected: Vec<&String> = environment
        .keys()
        .filter(|key| !["HOME", "PATH", "LC_ALL", "PWD", "SHLVL", "_", "OLDPWD"].contains(&key.as_str()))
        .collect();
    assert!(unexpected.is_empty(), "inherited {unexpected:?}");
    for failure in [
        json!({"schemaVersion": 1, "ok": false, "error": {"code": "outcomeUnknown"}}),
        json!({"schemaVersion": 2, "ok": true, "data": {}}),
        json!({"schemaVersion": 1, "ok": true}),
    ] {
        fs::write(directory.path().join("response"), failure.to_string()).unwrap();
        assert!(runtime.call(&["vm", "stop"], "default", &["--yes"]).is_err(), "{failure}");
    }
    fs::write(directory.path().join("response"), "not json").unwrap();
    assert!(runtime.call(&["vm", "status"], "default", &[]).is_err());
}

fn log_records_are_read_as_ndjson_and_failures_are_not_hidden() {
    let directory = TempDir::new("hamn-release-runtime-");
    let mut records = [
        json!({"schemaVersion": 1, "ok": true, "data": {"text": "hello"}}),
        json!({"schemaVersion": 1, "ok": true, "data": {"complete": true}}),
    ];
    let ndjson = |records: &[Value]| records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let runtime = recording_runtime(directory.path(), &ndjson(&records));
    assert_eq!(
        runtime.call(&["docker", "containers", "logs", "fixture"], "default", &[]).unwrap(),
        json!({"complete": true})
    );
    records[0]["ok"] = json!(false);
    fs::write(directory.path().join("response"), ndjson(&records)).unwrap();
    assert!(runtime.call(&["docker", "containers", "logs", "fixture"], "default", &[]).is_err());
    fs::write(directory.path().join("response"), "").unwrap();
    assert!(runtime.call(&["docker", "containers", "logs", "fixture"], "default", &[]).is_err());
}

fn stop_reports_every_profile_it_cannot_prove_stopped() {
    let directory = TempDir::new("hamn-release-runtime-");
    let binary = directory.path().join("hamn");
    fs::write(
        &binary,
        r#"#!/bin/sh
printf '%s %s\n' "$3" "$5" >>"$HOME/calls"
case "$3:$5" in
stop:third) echo '{"schemaVersion":1,"ok":false,"error":{"code":"outcomeUnknown"}}'; exit 1 ;;
stop:*) echo '{"schemaVersion":1,"ok":true,"data":{}}' ;;
status:default) echo '{"schemaVersion":1,"ok":true,"data":{"state":"stopped"}}' ;;
status:*) echo '{"schemaVersion":1,"ok":true,"data":{"state":"running"}}' ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let runtime = Runtime::new(&binary, directory.path(), "docker");
    let profiles = ["default", "second", "third", "fourth"].map(String::from);
    let error = runtime.stop(&profiles).unwrap_err();
    assert!(error.starts_with("physical test cleanup failed; workspace retained: "), "{error}");
    assert!(error.contains("second") && error.contains("failed (1)") && error.contains("fourth"), "{error}");
    assert!(!error.contains("default"), "{error}");
    // Every profile was attempted despite earlier failures.
    let calls = fs::read_to_string(directory.path().join("calls")).unwrap();
    assert_eq!(
        calls,
        "stop default\nstatus default\nstop second\nstatus second\nstop third\nstop fourth\nstatus fourth\n"
    );
    runtime.stop(&["default".to_owned()]).unwrap();
}

/// A fake TUI in `sh`: raw mode, render, read one key, restore.
fn terminal_runtime(directory: &Path, body: &str) -> Runtime {
    let binary = directory.join("tui");
    fs::write(&binary, format!("#!/bin/sh\nsaved=$(stty -g)\nstty raw -echo\n{body}\n")).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    Runtime::new(binary, directory, "docker")
}

const QUIT: &str = "key=$(dd bs=1 count=1 2>/dev/null)\n[ \"$key\" = q ] || exit 9";

fn terminal_quit_drains_output_larger_than_the_pty_queue() {
    let directory = TempDir::new("hamn-release-terminal-");
    let body = format!("printf Hamn\nhead -c 262144 /dev/zero | tr '\\000' x\n{QUIT}\nstty \"$saved\"");
    terminal_runtime(directory.path(), &body).terminal().unwrap();
}

fn terminal_requires_render_zero_exit_and_restored_settings() {
    let directory = TempDir::new("hamn-release-terminal-");
    terminal_runtime(directory.path(), &format!("printf Hamn\n{QUIT}\nstty \"$saved\"")).terminal().unwrap();
    let error = terminal_runtime(directory.path(), &format!("printf Hamn\n{QUIT}")).terminal().unwrap_err();
    assert!(error.contains("did not restore"), "{error}");
    let error = terminal_runtime(directory.path(), &format!("printf Hamn\n{QUIT}\nstty \"$saved\"\nexit 3"))
        .terminal()
        .unwrap_err();
    assert!(error.contains("did not restore"), "{error}");
    let started = std::time::Instant::now();
    let error = terminal_runtime(directory.path(), "printf nothing\nsleep 30")
        .terminal_within(Duration::from_millis(500), Duration::from_secs(1))
        .unwrap_err();
    assert!(error.contains("did not render"), "{error}");
    let error = terminal_runtime(directory.path(), &format!("printf Hamn\n{QUIT}\nsleep 30"))
        .terminal_within(Duration::from_secs(5), Duration::from_millis(500))
        .unwrap_err();
    assert!(error.contains("did not exit"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(20), "the fake TUIs were not killed");
}

fn environment(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: BTreeMap<String, String> = pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
    move |key| map.get(key).cloned()
}

fn invalid_network_mode_is_rejected_before_any_runtime_action() {
    let inputs = [
        ("HAMN_CANDIDATE_DIR", "/candidate"),
        ("HAMN_E2E_OUTPUT", "/output.json"),
        ("HAMN_E2E_CONTEXT", "test"),
        ("HAMN_E2E_KUBECONFIG", "/kubeconfig"),
    ];
    for value in ["true", "", "2", "01"] {
        let mut pairs = inputs.to_vec();
        pairs.push(("HAMN_E2E_K8S_HOST_NETWORK", value));
        assert!(Config::from_env(environment(&pairs)).unwrap_err().contains("HOST_NETWORK"), "{value:?}");
    }
    let config = Config::from_env(environment(&inputs)).unwrap();
    assert_eq!((config.host_network, config.run.as_str(), config.attempt.as_str()), (false, "local", "local"));
    let mut pairs = inputs.to_vec();
    pairs.extend([("HAMN_E2E_K8S_HOST_NETWORK", "1"), ("GITHUB_RUN_ID", "7"), ("GITHUB_RUN_ATTEMPT", "2")]);
    let config = Config::from_env(environment(&pairs)).unwrap();
    assert_eq!((config.host_network, config.run.as_str(), config.attempt.as_str()), (true, "7", "2"));
    let error =
        Config::from_env(environment(&[("HAMN_E2E_OUTPUT", "/output.json"), ("HAMN_E2E_CONTEXT", "")])).unwrap_err();
    assert_eq!(error, "missing physical validator inputs: HAMN_CANDIDATE_DIR, HAMN_E2E_CONTEXT, HAMN_E2E_KUBECONFIG");
    // The executable stops at the same check with an empty environment,
    // where no command or path could be used.
    let output = hamn_dev(&["release", "physical-e2e"], &[("HAMN_E2E_K8S_HOST_NETWORK", "true")]);
    assert!(!output.status.success() && String::from_utf8_lossy(&output.stderr).contains("HOST_NETWORK"), "{output:?}");
}

fn hamn_dev(args: &[&str], environment: &[(&str, &str)]) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args(args)
        .env_clear()
        .envs(environment.iter().copied())
        .output()
        .unwrap()
}

fn help_needs_no_validator_environment_or_runtime() {
    for help in ["--help", "-h"] {
        let output = hamn_dev(&["release", "physical-e2e", help], &[]);
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("usage: hamn-dev release physical-e2e"));
    }
    let output = hamn_dev(&["release", "external-kubernetes-e2e", "--help"], &[]);
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("--host-network"),
        "{output:?}"
    );
    let output = hamn_dev(&["release", "physical-e2e", "--unknown"], &[]);
    assert!(!output.status.success() && String::from_utf8_lossy(&output.stderr).contains("unrecognized"), "{output:?}");
}

fn kubernetes_harness_arguments_and_existing_output() {
    let words = |items: &[&str]| items.iter().map(|item| item.to_string()).collect::<Vec<_>>();
    let options = kubernetes::parse(&words(&[
        "--hamn",
        "/bin/hamn",
        "--context=test",
        "--output",
        "/out.json",
        "--host-network",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(
        options,
        kubernetes::Options {
            hamn: "/bin/hamn".into(),
            context: "test".into(),
            kubeconfig: None,
            output: "/out.json".into(),
            host_network: true
        }
    );
    assert_eq!(kubernetes::parse(&words(&["--help"])).unwrap(), None);
    for invalid in [
        &["--context", "test", "--output", "/out.json"][..],
        &["--hamn", "/bin/hamn", "--context"],
        &["--hamn", "/bin/hamn", "--context", "test", "--output", "/o", "--unknown"],
        &["--hamn", "/bin/hamn", "--context", "test", "--output", "/o", "--host-network=1"],
    ] {
        assert!(kubernetes::parse(&words(invalid)).is_err(), "{invalid:?}");
    }
    // Existing evidence is never replaced, and nothing touches a cluster:
    // kubectl is not even on PATH here.
    let directory = TempDir::new("hamn-release-kubernetes-");
    let output = directory.path().join("kubernetes.json");
    fs::write(&output, "previous").unwrap();
    let binary = std::env::current_exe().unwrap();
    let result = hamn_dev(
        &[
            "release",
            "external-kubernetes-e2e",
            "--hamn",
            binary.to_str().unwrap(),
            "--context",
            "test",
            "--output",
            output.to_str().unwrap(),
        ],
        &[("PATH", "/nonexistent")],
    );
    assert!(
        !result.status.success() && String::from_utf8_lossy(&result.stderr).contains("already exists"),
        "{result:?}"
    );
    assert_eq!(fs::read(&output).unwrap(), b"previous");
}
