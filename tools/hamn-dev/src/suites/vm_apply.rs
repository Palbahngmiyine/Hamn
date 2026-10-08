//! `vm apply` through the headless contract of the real binary, with an
//! isolated HOME: a profile definition file creates a profile, replaces
//! differing settings or changes nothing, and every refusal leaves the
//! stored configuration byte for byte. Never starts a VM; the C tests cover
//! a running one.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, py_json};
use crate::support::hamn;
use crate::support::tmp::TempDir;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "vm-apply",
        "a profile definition file is applied exactly or refused without a change",
        vec![
            case("a_definition_creates_a_profile_and_a_repeat_changes_nothing", a_definition_creates_a_profile_and_a_repeat_changes_nothing),
            case("omitted_settings_return_to_defaults_and_every_change_is_listed", omitted_settings_return_to_defaults_and_every_change_is_listed),
            case("a_dry_run_needs_no_confirmation_and_writes_nothing", a_dry_run_needs_no_confirmation_and_writes_nothing),
            case("refused_requests_and_definitions_create_nothing", refused_requests_and_definitions_create_nothing),
            case("states_that_forbid_a_change_leave_the_configuration", states_that_forbid_a_change_leave_the_configuration),
            case("a_failed_write_is_reported_by_what_it_left", a_failed_write_is_reported_by_what_it_left),
            case("a_first_start_cut_short_does_not_block_the_profile", a_first_start_cut_short_does_not_block_the_profile),
        ],
        filters,
    )
}

/// A profile definition whose `spec` is `spec`, indented by two spaces, or
/// an empty mapping.
fn definition(name: &str, spec: &str) -> String {
    let spec = if spec.is_empty() {
        " {}\n".to_owned()
    } else {
        format!("\n{}", spec.lines().map(|line| format!("  {line}\n")).collect::<String>())
    };
    format!("apiVersion: hamn/v1\nkind: Profile\nmetadata:\n  name: {name}\nspec:{spec}")
}

/// The binary under test with a private HOME, and one definition file.
struct Home {
    directory: TempDir,
}

impl Home {
    fn new() -> Self {
        Self { directory: TempDir::new("hamn-apply-") }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn profile(&self, name: &str) -> PathBuf {
        self.path().join(".hamn").join(name)
    }

    fn config(&self, name: &str) -> PathBuf {
        self.profile(name).join("config.yaml")
    }

    /// Writes `text` as the definition file and returns its path.
    fn write(&self, text: &str) -> String {
        let path = self.path().join("definition.yaml");
        fs::write(&path, text).unwrap();
        path.display().to_string()
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(hamn());
        command.arg("--headless").args(arguments).env("HOME", self.path()).current_dir(self.path());
        // A fault that the caller's environment names must not reach a case
        // that does not inject one.
        command.env_remove("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH").env_remove("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE");
        command
    }

    /// `hamn --headless ARGUMENTS`: its envelope, whose `ok` matches the
    /// exit status (0, or 1 for a failed operation).
    fn run(&self, command: &mut Command) -> Value {
        let result = api_fixtures::run(command, None, Duration::from_secs(15));
        let value = result.json();
        assert_eq!(result.status.code(), Some(if value["ok"] == true { 0 } else { 1 }), "{result:?}");
        value
    }

    /// `vm apply --profile NAME --file <text> EXTRA`.
    fn apply(&self, name: &str, text: &str, extra: &[&str]) -> Value {
        let file = self.write(text);
        self.run(&mut self.command(&[&["vm", "apply", "--profile", name, "--file", &file][..], extra].concat()))
    }

    /// An apply that succeeds: its `data`.
    fn applied(&self, name: &str, text: &str, extra: &[&str]) -> Value {
        let value = self.apply(name, text, extra);
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(value["target"]["profile"], name, "{value}");
        value["data"].clone()
    }

    /// An apply that fails with `code` and a message that contains `reason`.
    fn refused(&self, name: &str, text: &str, extra: &[&str], code: &str, reason: &str) {
        let value = self.apply(name, text, extra);
        assert!(value["ok"] == false && value["data"].is_null(), "{value}");
        assert_eq!(value["error"]["code"], code, "{value}");
        assert!(value["error"]["message"].as_str().is_some_and(|message| message.contains(reason)), "{reason}: {value}");
    }

    fn status(&self, name: &str) -> Value {
        let value = self.run(&mut self.command(&["vm", "status", "--profile", name]));
        assert_eq!(value["ok"], true, "{value}");
        value["data"].clone()
    }

    /// The profiles that `vm list` shows.
    fn listed(&self) -> Vec<String> {
        let value = self.run(&mut self.command(&["vm", "list"]));
        value["data"].as_array().unwrap_or_else(|| panic!("{value}")).iter().map(|row| row["name"].as_str().unwrap().to_owned()).collect()
    }
}

fn a_definition_creates_a_profile_and_a_repeat_changes_nothing() {
    let home = Home::new();
    let text = definition(
        "work",
        "cpus: 6\nmemoryMiB: 6000\ndiskGiB: 80\nrosetta: true\nmounts:\n  - location: /Volumes/data\n    mountPoint: /data\nprovision:\n  - command: echo ready",
    );
    let created = home.applied("work", &text, &["--yes"]);
    assert_eq!(created, json!({"profile": "work", "action": "create", "changes": [], "dryRun": false}));
    let config = home.config("work");
    assert_eq!(fs::metadata(&config).unwrap().permissions().mode() & 0o7777, 0o600);
    assert_eq!(fs::metadata(home.profile("work")).unwrap().permissions().mode() & 0o7777, 0o700);
    let status = home.status("work");
    assert!(status["cpus"] == 6 && status["memoryMiB"] == 6000 && status["diskGiB"] == 80 && status["rosetta"] == true, "{status}");
    assert_eq!(status["state"], "stopped", "{status}");
    assert_eq!(status["lastOperation"], Value::Null, "{status}");
    assert_eq!(status["sharedDirectories"][1], json!({"hostPath": "/Volumes/data", "guestPath": "/data", "writable": false}), "{status}");
    assert_eq!(home.listed(), ["work"]);

    // The same definition again writes nothing: same bytes, inode and
    // modification time, by a relative path as well.
    let before = fs::read(&config).unwrap();
    let past = fs::File::open(&config).unwrap();
    past.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000)).unwrap();
    let inode = fs::metadata(&config).unwrap().ino();
    let unchanged = json!({"profile": "work", "action": "none", "changes": [], "dryRun": false});
    assert_eq!(home.applied("work", &text, &["--yes"]), unchanged);
    home.write(&text);
    let relative = home.run(&mut home.command(&["vm", "apply", "--profile", "work", "--file", "definition.yaml", "--yes"]));
    assert_eq!(relative["data"], unchanged, "{relative}");
    let metadata = fs::metadata(&config).unwrap();
    assert!(metadata.ino() == inode && metadata.mtime() == 1_000_000_000, "{metadata:?}");
    assert_eq!(fs::read(&config).unwrap(), before);
    // The operation takes the worker's own request form as well.
    let request = json!({"words": ["vm", "apply"], "timeout": 30, "tail": 200, "profile": "work", "file": home.write(&text), "yes": true});
    let mut worker = Command::new(hamn());
    worker.arg("__core-worker").env("HOME", home.path());
    let result = api_fixtures::run(&mut worker, Some(py_json(&request).as_bytes()), Duration::from_secs(15));
    assert!(result.success(), "{result:?}");
    assert_eq!(result.json(), json!({"Ok": unchanged}));
}

fn omitted_settings_return_to_defaults_and_every_change_is_listed() {
    let home = Home::new();
    home.applied(
        "work",
        &definition("work", "cpus: 6\nrosetta: true\ndocker:\n  daemonJson: '{\"debug\":true}'\nprovision:\n  - command: echo ready"),
        &["--yes"],
    );
    // The definition is the whole configuration: the keys this one leaves
    // out go back to their defaults. Numbers and booleans carry their
    // values; a hook command or the Docker settings can hold a credential,
    // so those are named only.
    let second = definition("work", "cpus: 8\nsshAgent: true\nmounts:\n  - location: /Volumes/data\n    mountPoint: /data\n    writable: true");
    let changes = json!([
        {"key": "cpus", "from": 6, "to": 8},
        {"key": "docker.daemonJson"},
        {"key": "rosetta", "from": true, "to": false},
        {"key": "sshAgent", "from": false, "to": true},
        {"key": "mounts"},
        {"key": "provision"},
    ]);
    let before = fs::read(home.config("work")).unwrap();
    assert_eq!(home.applied("work", &second, &["--dry-run"]), json!({"profile": "work", "action": "configure", "changes": changes, "dryRun": true}));
    assert_eq!(fs::read(home.config("work")).unwrap(), before);
    assert_eq!(home.applied("work", &second, &["--yes"]), json!({"profile": "work", "action": "configure", "changes": changes, "dryRun": false}));
    let text = fs::read_to_string(home.config("work")).unwrap();
    for line in ["cpus: 8\n", "rosetta: false\n", "sshAgent: true\n", "daemonJson: \"\"\n", "provision: []\n", "    writable: true\n"] {
        assert!(text.contains(line), "{line:?} in {text}");
    }
    assert_eq!(fs::metadata(home.config("work")).unwrap().permissions().mode() & 0o7777, 0o600);
    assert_eq!(home.applied("work", &second, &["--yes"])["action"], "none");
    // Settings compare by meaning: a hand-written file that holds the same
    // settings is left alone, with its comment.
    let by_hand = "# kept by hand\nsshAgent: true\ncpus: 8\nmounts:\n  - mountPoint: /data\n    location: /Volumes/data\n    writable: true\n";
    fs::write(home.config("work"), by_hand).unwrap();
    assert_eq!(home.applied("work", &second, &["--yes"])["action"], "none");
    assert_eq!(fs::read_to_string(home.config("work")).unwrap(), by_hand);
}

fn a_dry_run_needs_no_confirmation_and_writes_nothing() {
    let home = Home::new();
    let text = definition("work", "cpus: 2");
    assert_eq!(home.applied("work", &text, &["--dry-run"]), json!({"profile": "work", "action": "create", "changes": [], "dryRun": true}));
    assert!(!home.path().join(".hamn").exists());
    // Without --dry-run the mutation needs --yes, and is refused before the
    // worker runs.
    home.refused("work", &text, &[], "invalidRequest", "mutation requires --yes");
    assert!(!home.path().join(".hamn").exists());
    // A dry run is one answer: it cannot be repeated.
    home.refused("work", &text, &["--dry-run", "--watch"], "invalidRequest", "vm apply cannot be repeated or streamed");
    home.refused("work", &text, &["--yes", "--watch"], "invalidRequest", "vm apply cannot be repeated or streamed");
    assert!(!home.path().join(".hamn").exists());
}

/// The malformed settings of the profile-yaml suite, as `spec`: a definition
/// is not read more loosely than config.yaml.
const SPECS: &[(&str, &str)] = &[
    ("unknown: true", "unknown configuration key: unknown"),
    ("cpus: 4\ncpus: 5", "duplicate or invalid configuration key"),
    ("cpus: &cpu 4", "YAML anchors and tags are not supported"),
    ("cpus: *cpu", "YAML aliases are not supported"),
    ("cpus: !!int 4", "YAML anchors and tags are not supported"),
    ("base: &base { cpus: 4 }\n<<: *base", "YAML anchors and tags are not supported"),
    ("mountHome: \"true\"", "invalid scalar value"),
    ("mounts: true", "expected a sequence"),
    ("mounts:\n  - location: relative\n    mountPoint: /workspace\n    writable: false", "a mount location must be a normalized absolute path"),
    ("provision:\n  - command: echo ready\n    stage: invalid", "a provision stage must be system, user, after-boot or ready"),
    ("network:\n  mode: shared", "unknown configuration key: network"),
    ("kubernetes:\n  enabled: true", "unknown configuration key: kubernetes"),
    ("mountInotify: \"true\"", "invalid scalar value"),
    ("mountHome: false\nmountInotify: true", "mountInotify requires a writable share"),
    ("docker:\n  daemonJson: \"[\"", "docker.daemonJson must be one JSON object"),
    ("docker:\n  daemonJson: \"{\\\"containerd\\\":\\\"/other.sock\\\"}\"", "docker.daemonJson must be one JSON object"),
    // What config.yaml cannot store is refused before it is written.
    ("provision:\n  - command: \"echo \\x7F\"", "a setting holds a character that config.yaml cannot store"),
    ("provision:\n  - command: \"echo \\a\"", "a setting holds a character that config.yaml cannot store"),
];

fn refused_requests_and_definitions_create_nothing() {
    let home = Home::new();
    for (spec, reason) in SPECS {
        for extra in [&["--yes"][..], &["--dry-run"]] {
            home.refused("work", &definition("work", spec), extra, "invalidRequest", reason);
        }
    }
    let valid = definition("work", "");
    for (text, reason) in [
        (String::new(), "expected exactly one YAML configuration document"),
        ("cpus: 4\n".to_owned(), "unknown definition key: cpus"),
        (valid.replace("hamn/v1", "hamn/v2"), "apiVersion must be hamn/v1"),
        (valid.replace("Profile", "VirtualMachine"), "kind must be Profile"),
        (valid.replace("  name: work\n", "  name: work\n  labels: {}\n"), "unknown metadata key: labels"),
        (valid.replace("metadata:\n  name: work\n", ""), "a profile definition requires apiVersion, kind, metadata.name and spec"),
        (format!("{valid}---\n{valid}"), "expected exactly one YAML configuration document"),
        // A key is quoted in printable ASCII: no escape sequence of the file
        // reaches the terminal.
        (format!("{valid}\"\\e[2Jtitle\": 1\n"), "unknown definition key: ?[2Jtitle"),
        (definition("other", ""), "the definition names profile other but --profile is work"),
    ] {
        home.refused("work", &text, &["--yes"], "invalidRequest", reason);
    }
    // The file itself, and the arguments.
    let file = home.write(&valid);
    for (arguments, reason) in [
        (vec!["vm", "apply", "--profile", "work", "--yes"], "vm apply requires --file <path>"),
        (vec!["vm", "apply", "--profile", "work", "--file", "-", "--yes"], "standard input (-) is not supported"),
        (vec!["vm", "apply", "--profile", "work", "--file", "missing.yaml", "--yes"], "cannot read the profile definition missing.yaml"),
        (vec!["vm", "apply", "--profile", "work", "--file", ".", "--yes"], "is not a regular file"),
        (vec!["vm", "apply", "--file", &file, "--yes"], "an explicit --profile is required"),
        (vec!["vm", "apply", "--profile", "work", "--file", &file, "--cpu", "2", "--yes"], "--cpu, --memory and --disk cannot be combined with vm apply"),
        (vec!["vm", "apply", "--profile", "work", "--file", &file, "--rosetta", "true", "--yes"], "--rosetta is only supported for vm create and vm configure"),
        (vec!["vm", "apply", &file, "--profile", "work", "--yes"], "pass the profile definition with --file <path>"),
        (vec!["vm", "status", "--profile", "work", "--file", &file], "--file and --dry-run are only supported for vm apply"),
        (vec!["vm", "status", "--profile", "work", "--dry-run"], "--file and --dry-run are only supported for vm apply"),
    ] {
        let value = home.run(&mut home.command(&arguments));
        assert_eq!(value["error"]["code"], "invalidRequest", "{arguments:?}: {value}");
        assert!(value["error"]["message"].as_str().is_some_and(|message| message.contains(reason)), "{arguments:?}: {value}");
    }
    let oversized = format!("{valid}{}", "#".repeat(65537 - valid.len()));
    home.refused("work", &oversized, &["--yes"], "invalidRequest", "is larger than 65536 bytes");
    // None of this created anything, and at the size limit a definition is read.
    assert!(!home.path().join(".hamn").exists());
    assert_eq!(home.applied("work", &oversized[..65536], &["--dry-run"])["action"], "create");
    assert!(!home.path().join(".hamn").exists());
}

fn states_that_forbid_a_change_leave_the_configuration() {
    let home = Home::new();
    // The disk never shrinks, and a definition without diskGiB asks for 60.
    home.applied("grown", &definition("grown", "diskGiB: 80"), &["--yes"]);
    let before = fs::read(home.config("grown")).unwrap();
    for extra in [&["--yes"][..], &["--dry-run"]] {
        home.refused("grown", &definition("grown", "cpus: 2"), extra, "conflict", "disk size cannot shrink (current: 80 GiB); set spec.diskGiB to 80 or more");
    }
    assert_eq!(fs::read(home.config("grown")).unwrap(), before);

    // A deleted profile stays deleted, also for equal settings.
    let gone = definition("gone", "cpus: 2");
    home.applied("gone", &gone, &["--yes"]);
    assert_eq!(home.run(&mut home.command(&["vm", "delete", "--profile", "gone", "--yes"]))["ok"], true);
    let before = fs::read(home.config("gone")).unwrap();
    for extra in [&["--yes"][..], &["--dry-run"]] {
        home.refused("gone", &gone, extra, "conflict", "profile gone is deleted");
    }
    assert_eq!(fs::read(home.config("gone")).unwrap(), before);
    assert!(home.profile("gone").join("deleted").is_file());
    assert_eq!(home.listed(), ["grown"]);

    // A directory that holds files but no configuration is not a profile.
    fs::create_dir(home.profile("diagnostics")).unwrap();
    fs::write(home.profile("diagnostics").join("archive.tar"), b"archive").unwrap();
    home.refused("diagnostics", &definition("diagnostics", ""), &["--yes"], "conflict", "holds files but no config.yaml");
    assert!(!home.config("diagnostics").exists());
    // A directory with nothing but what a creation leaves behind when it is
    // interrupted or refused becomes the profile: no entry at all, or the
    // record of a first start that was refused before it stored a
    // configuration (here for a disk below the default size).
    fs::create_dir(home.profile("interrupted")).unwrap();
    assert_eq!(home.applied("interrupted", &definition("interrupted", ""), &["--yes"])["action"], "create");
    let refused = home.run(&mut home.command(&["vm", "start", "--profile", "unstarted", "--disk", "40", "--yes"]));
    assert_eq!(refused["ok"], false, "{refused}");
    let left: Vec<_> = fs::read_dir(home.profile("unstarted")).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(left, ["operation.json"], "what the refused start left");
    assert_eq!(home.applied("unstarted", &definition("unstarted", "diskGiB: 40"), &["--yes"])["action"], "create");
    assert_eq!(home.status("unstarted")["diskGiB"], 40);

    // A stored configuration that cannot be read is not replaced.
    fs::create_dir(home.profile("broken")).unwrap();
    fs::write(home.config("broken"), "network: shared\n").unwrap();
    home.refused("broken", &definition("broken", ""), &["--yes"], "operationFailed", "cannot read the configuration of profile broken: unknown configuration key: network");
    assert_eq!(fs::read_to_string(home.config("broken")).unwrap(), "network: shared\n");
    fs::remove_dir_all(home.profile("broken")).unwrap();

    // A VM whose ownership cannot be verified is not a stopped VM: a change
    // is refused, and not as a state that stopping would resolve. Equal
    // settings do not look at the VM.
    let unsure = definition("unsure", "cpus: 2");
    home.applied("unsure", &unsure, &["--yes"]);
    fs::write(home.profile("unsure").join("vmrun.pid"), "2147483646\n").unwrap();
    fs::write(home.profile("unsure").join("vmrun.identity"), "not an identity\n").unwrap();
    let before = fs::read(home.config("unsure")).unwrap();
    home.refused("unsure", &definition("unsure", "cpus: 4"), &["--yes"], "operationFailed", "cannot verify the VM process of profile unsure; settings were not changed");
    assert_eq!(fs::read(home.config("unsure")).unwrap(), before);
    assert_eq!(home.applied("unsure", &unsure, &["--yes"])["action"], "none");
    assert_eq!(home.applied("unsure", &definition("unsure", "cpus: 4"), &["--dry-run"])["action"], "configure");

    // A refusal is classified by itself: an unresolved earlier operation of
    // the profile does not turn it into an unknown outcome.
    let record = home.profile("grown").join("operation.json");
    let rejected = home.run(&mut home.command(&["vm", "start", "--profile", "grown", "--disk", "10", "--yes"]));
    assert_eq!(rejected["error"]["code"], "operationFailed", "{rejected}");
    let mut value = api_fixtures::parse(&fs::read_to_string(&record).unwrap());
    value["status"] = json!("outcomeUnknown");
    fs::write(&record, py_json(&value)).unwrap();
    home.refused("grown", &definition("grown", "unknown: true"), &["--yes"], "invalidRequest", "unknown configuration key: unknown");
    home.refused("grown", &definition("grown", "cpus: 2"), &["--yes"], "conflict", "disk size cannot shrink");
    assert_eq!(home.applied("grown", &definition("grown", "diskGiB: 81"), &["--yes"])["changes"], json!([{"key": "diskGiB", "from": 80, "to": 81}]));
    assert_eq!(home.status("grown")["lastOperation"]["status"], "outcomeUnknown");
}

fn a_failed_write_is_reported_by_what_it_left() {
    let home = Home::new();
    let fault = |name: &str, stage: &str, text: &str| {
        let file = home.write(text);
        let mut command = home.command(&["vm", "apply", "--profile", name, "--file", &file, "--yes"]);
        // The path as the C core spells it: HOME, then the profile.
        command.env("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH", format!("{}/.hamn/{name}/config.yaml", home.path().display()));
        command.env("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE", stage);
        home.run(&mut command)
    };
    // The write fails before the file is replaced. Nothing is left of a
    // profile that the call was creating.
    let failed = fault("work", "open", &definition("work", "cpus: 2"));
    assert_eq!(failed["error"]["code"], "operationFailed", "{failed}");
    assert!(failed["error"]["message"].as_str().unwrap().contains("cannot save settings"), "{failed}");
    assert!(!home.profile("work").exists());
    assert_eq!(home.applied("work", &definition("work", "cpus: 2"), &["--yes"])["action"], "create");
    // An existing configuration keeps every byte.
    let before = fs::read(home.config("work")).unwrap();
    let failed = fault("work", "open", &definition("work", "cpus: 4"));
    assert_eq!(failed["error"]["code"], "operationFailed", "{failed}");
    assert_eq!(fs::read(home.config("work")).unwrap(), before);

    // The write fails after the file is replaced: the new settings are in
    // place, and the result says that the outcome must be observed.
    let failed = fault("work", "fsync", &definition("work", "cpus: 4"));
    assert_eq!(failed["error"]["code"], "outcomeUnknown", "{failed}");
    assert!(failed["error"]["message"].as_str().unwrap().contains("written but not confirmed durable"), "{failed}");
    assert_eq!(home.status("work")["cpus"], 4);
    assert_eq!(home.applied("work", &definition("work", "cpus: 4"), &["--yes"])["action"], "none");
    // The same failure while creating leaves the profile, for the same reason.
    let failed = fault("late", "fsync", &definition("late", "cpus: 2"));
    assert_eq!(failed["error"]["code"], "outcomeUnknown", "{failed}");
    assert_eq!(home.status("late")["cpus"], 2);
    let mut listed = home.listed();
    listed.sort();
    assert_eq!(listed, ["late", "work"]);
}

/// A first `vm start` writes its record, its state and the locks of its
/// cleanup before it stores the configuration. When it ends there (here the
/// write fails), the directory holds those and no `config.yaml`. That is no
/// profile yet and nothing of a VM: a definition creates the profile in it.
fn a_first_start_cut_short_does_not_block_the_profile() {
    let home = Home::new();
    let mut start = home.command(&["vm", "start", "--profile", "cut", "--yes"]);
    start.env("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH", format!("{}/.hamn/cut/config.yaml", home.path().display()));
    start.env("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE", "open");
    // Past that write a start prepares a guest image through the executable
    // it was invoked as. Should the write ever not fail, this name ends the
    // start there instead of letting it reach the network.
    start.arg0(home.path().join("no-such-hamn"));
    let cut = home.run(&mut start);
    assert!(cut["error"]["message"].as_str().is_some_and(|message| message.contains("cannot save profile config")), "{cut}");
    let mut left: Vec<String> =
        fs::read_dir(home.profile("cut")).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert!(left.contains(&"operation.json".to_owned()) && left.len() > 1 && !left.contains(&"config.yaml".to_owned()), "{left:?}");

    let text = definition("cut", "cpus: 2\ndiskGiB: 20");
    assert_eq!(home.applied("cut", &text, &["--dry-run"])["action"], "create");
    assert_eq!(home.applied("cut", &text, &["--yes"])["action"], "create");
    let status = home.status("cut");
    assert!(status["cpus"] == 2 && status["diskGiB"] == 20 && status["state"] == "stopped", "{status}");
    assert_eq!(home.applied("cut", &text, &["--yes"])["action"], "none");
    // A disk in such a directory is data: still not adopted.
    let mut start = home.command(&["vm", "start", "--profile", "disk", "--yes"]);
    start.env("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH", format!("{}/.hamn/disk/config.yaml", home.path().display()));
    start.env("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE", "open");
    start.arg0(home.path().join("no-such-hamn"));
    assert_eq!(home.run(&mut start)["ok"], false);
    fs::write(home.profile("disk").join("disk.img"), b"data").unwrap();
    home.refused("disk", &definition("disk", ""), &["--yes"], "conflict", "holds files but no config.yaml");
    assert!(!home.config("disk").exists());
}
