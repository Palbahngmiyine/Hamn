//! Disposable PTY contract tests of workspaces: default selection and its
//! persistence, native CLI dispatch exactly once, interactive input and
//! detach keys through the TUI, and terminal restoration.
use crate::runner::{self, case};
use crate::support::exec::{self, Session};
use crate::support::pty::{self, Pty};
use crate::support::screen::RatatuiScreen;
use crate::support::termios;
use crate::support::tui::install_fixture;
use crate::support::{hamn, tmp::TempDir};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "tui-workspaces",
        "workspace persistence, isolated scopes, native output, PTY input/detach and exact-once CLI dispatch",
        // The runs share one HOME: later runs start from the saved choice.
        vec![case("run/first+saved+calls+terminate_cli+exited", runs)],
        filters,
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Terminate {
    No,
    /// SIGTERM while the CLI waits for input (`terminate_cli=True`).
    Waiting,
    /// SIGTERM after the CLI read its input and exited
    /// (`terminate_cli='exited'`).
    Exited,
}

fn runs() {
    let directory = TempDir::new("hamn-workspaces-");
    let root = directory.path();
    let tools = root.join("bin");
    fs::create_dir(&tools).unwrap();
    for name in ["docker", "kubectl", "kubectl-hamnfixture"] {
        install_fixture(&tools, name);
    }
    let config = root.join("kubeconfig");
    fs::write(
        &config,
        json!({"apiVersion": "v1", "kind": "Config", "current-context": "dev",
            "contexts": [{"name": "dev", "context": {"cluster": "dev", "namespace": "test"}}],
            "clusters": [{"name": "dev", "cluster": {"server": "http://127.0.0.1:1"}}]})
        .to_string(),
    )
    .unwrap();
    run(root, false, Terminate::No);
    run(root, true, Terminate::No);
    let calls: Vec<(String, Vec<String>)> =
        fs::read_to_string(root.join("calls")).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let with = |arg: &str| calls.iter().filter(|(_, args)| args.iter().any(|value| value == arg)).count();
    assert_eq!(with("-q"), 1, "{calls:?}");
    assert_eq!(with("exec"), 1, "{calls:?}");
    let plugin: Vec<&Vec<String>> =
        calls.iter().map(|(_, args)| args).filter(|args| args.iter().any(|arg| arg == "hamnfixture")).collect();
    assert_eq!(plugin, [&vec!["hamnfixture".to_owned(), "--custom-option".into(), "value".into()]], "{calls:?}");
    assert!(
        calls
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "-q" || arg == "exec"))
            .all(|(_, args)| !args.iter().any(|arg| arg == "--format"))
    );
    assert!(only_preferences(root), "TUI entry created VM state");
    run(root, true, Terminate::Waiting);
    run(root, true, Terminate::Exited);
}

fn only_preferences(root: &Path) -> bool {
    let mut names: Vec<String> = fs::read_dir(root.join(".hamn"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names == ["tui.json"] || names == ["tui.json", "tui.lock"]
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// The TUI's PTY output: the raw bytes since the last `send`, and the
/// completed Ratatui frames.
struct Tui {
    master: i32,
    output: Vec<u8>,
    screen: RatatuiScreen,
}

impl Tui {
    fn until(&mut self, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.screen.text().contains(marker) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero() && !pty::readable(&[self.master], left).is_empty(),
                "{marker:?} {}",
                String::from_utf8_lossy(&self.output[self.output.len().saturating_sub(3000)..])
            );
            // Fragment redraws deliberately: a text marker alone must not
            // accept old rows remaining in a partially drawn settings view.
            let data = read_at_most(self.master, 64);
            self.output.extend_from_slice(&data);
            self.screen.feed(&data);
        }
    }

    fn send(&mut self, data: &[u8], marker: &str) {
        self.output.clear();
        pty::write_all(self.master, data);
        self.until(marker);
    }
}

/// One TUI session in `root`: the first run chooses and changes workspaces;
/// saved runs start in the saved workspace, optionally ending with SIGTERM.
fn run(root: &Path, saved: bool, terminate: Terminate) {
    let pty = Pty::open(32, 140);
    let before = termios::settings(pty.slave.as_raw_fd());
    let mut command = Command::new(hamn());
    command
        .env("HOME", root)
        .env("PATH", format!("{}:/usr/bin:/bin", root.join("bin").display()))
        .env("TERM", "xterm-256color")
        .env("KUBECONFIG", root.join("kubeconfig"))
        .env("CLI_RECORD", root.join("calls"))
        .env("HAMN_DEV_FIXTURE", "tui-workspaces");
    let mut session = Session(pty.spawn(&mut command));
    let mut tui = Tui { master: pty.master.as_raw_fd(), output: Vec::new(), screen: RatatuiScreen::new(32, 140) };
    if saved {
        tui.until("fixture-pod");
        assert!(!contains(&tui.output, b"Choose your default"));
        assert!(!contains(&tui.output, b"Hamn profile") && !contains(&tui.output, b"VM settings"));
        if terminate != Terminate::No {
            tui.send(b":docker exec -it fixture-container sh\r", "INPUT_READY");
            if terminate == Terminate::Exited {
                tui.send(b"xy", "DETACH_BYTES:7879");
                tui.until("Exit code 0");
            }
            pty::kill(session.id(), libc::SIGTERM);
        }
    } else {
        tui.until("Choose your default workspace");
        assert!(!root.join(".hamn").exists());
        tui.send(b"1\r", "fixture-container");
        let preferences = root.join(".hamn/tui.json");
        let saved: Value = serde_json::from_str(&fs::read_to_string(&preferences).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({"version": 1, "defaultWorkspace": "containers", "recentTargets": [{"kind": "hamn", "name": "default"}]})
        );
        assert_eq!(fs::metadata(&preferences).unwrap().permissions().mode() & 0o777, 0o600);
        tui.send(b"e", "external");
        tui.send(b"\r", "Docker context external");
        tui.until("fixture-container");
        pty::write_all(tui.master, b"v");
        tui.send(b":docker ps -q\r", "RAW_OUTPUT");
        tui.until("Exit code 7");
        tui.send(b"\r", "fixture-container");
        tui.send(b":exec -it fixture-container sh\r", "INPUT_READY");
        tui.send(b"\x10\x11", "DETACH_BYTES:1011");
        tui.until("Exit code 0");
        tui.send(b"\r", "fixture-container");
        tui.send(b"\t", "fixture-pod");
        tui.send(b":hamnfixture --custom-option value\r", "CLI_PASSTHROUGH");
        tui.until("Exit code 0");
        tui.send(b"\r", "fixture-pod");
        tui.send(b",", "Choose the workspace");
        tui.send(b"2\r", "fixture-pod");
        let saved: Value = serde_json::from_str(&fs::read_to_string(&preferences).unwrap()).unwrap();
        assert_eq!(saved["defaultWorkspace"], "kubernetes");
    }
    if terminate == Terminate::No {
        pty::write_all(tui.master, b"q");
    }
    let status = pty::wait_for_exit(&mut session.0, tui.master, Duration::from_secs(5)).expect("exit within 5 s");
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert_eq!(termios::settings(pty.slave.as_raw_fd()), before);
}

/// One `read` of at most `limit` bytes, like Python's `os.read(fd, limit)`.
fn read_at_most(fd: i32, limit: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; limit];
    loop {
        // SAFETY: buffer is writable for `limit` bytes.
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), limit) };
        if count >= 0 {
            buffer.truncate(count as usize);
            return buffer;
        }
        let error = std::io::Error::last_os_error();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted, "read: {error}");
    }
}

/// The recorded `docker`/`kubectl` peer (and the `kubectl-hamnfixture`
/// plugin, which only exits 0), with the former Python fixture's exit
/// statuses: an uncaught failure exits 1.
pub fn fixture(program: &str, args: &[String]) -> ExitCode {
    exec::python_exit(|| {
        if program == "kubectl-hamnfixture" {
            return ExitCode::SUCCESS;
        }
        let record = std::env::var_os("CLI_RECORD").expect("CLI_RECORD");
        let mut out = OpenOptions::new().create(true).append(true).open(record).unwrap();
        writeln!(out, "{}", json!([program, args])).unwrap();
        drop(out);
        let has = |value: &str| args.iter().any(|arg| arg == value);
        if has("ps") {
            if has("-q") {
                println!("RAW_OUTPUT");
                return ExitCode::from(7);
            }
            println!("{}", json!({"ID": "abc123", "Names": "fixture-container", "State": "running"}));
        } else if has("exec") {
            // SAFETY: isatty only inspects the descriptors.
            let terminals = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 && libc::isatty(2) == 1 };
            assert!(terminals, "exec streams are not terminals");
            termios::set_raw(0).expect("raw mode");
            println!("INPUT_READY");
            let mut data = Vec::new();
            while data.len() < 2 {
                // Like the former os.read loop: an error fails, while
                // end-of-file reads nothing and the loop reads again.
                data.extend(read_at_most(0, 2 - data.len()));
            }
            let hex: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
            println!("DETACH_BYTES:{hex}");
        } else if has("context") {
            println!("{}", json!({"Name": "external", "DockerEndpoint": "unix:///external/docker.sock", "Current": true}));
        } else if has("get") {
            println!("{}", json!({"items": [{"metadata": {"name": "fixture-pod", "namespace": "test", "uid": "uid1"}}]}));
        } else {
            println!("CLI_PASSTHROUGH");
        }
        ExitCode::SUCCESS
    })
}
