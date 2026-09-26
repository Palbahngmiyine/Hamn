//! TUI entry, navigation, resize and terminal restoration on a real PTY. The
//! tests wait for output readiness, not sleeps, and always reap the whole
//! session. The panic and coalesced-event cases run ignored PTY tests from
//! control/tui.rs in the product's `cargo test` binary.
use crate::runner::{self, Case, case};
use crate::support::exec::{self, Session};
use crate::support::pty::{self, Pty};
use crate::support::screen::Screen;
use crate::support::termios;
use crate::support::{hamn, tmp::TempDir};
use serde_json::Value;
use std::cell::OnceCell;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = Vec::new();
    for mode in ["q", "confirm-small", "interrupt", "terminate", "suspend-key", "suspend-signal"] {
        cases.push(case(format!("exercise/{mode}"), move || exercise(mode, None)));
    }
    // One `cargo test` build serves both cases, after the TUI cases as before.
    let test_binary: Rc<OnceCell<PathBuf>> = Rc::default();
    let binary = Rc::clone(&test_binary);
    cases.push(case("exercise/panic", move || {
        let binary = binary.get_or_init(build_test_binary);
        let mut command = Command::new(binary);
        command.args(["tui::tests::panic_restores_terminal_fixture", "--ignored", "--exact", "--nocapture"]);
        exercise("panic", Some(command));
    }));
    cases.push(case("coalesced_events", move || coalesced_events(test_binary.get_or_init(build_test_binary))));
    runner::run("tui", "TUI entry, navigation, resize and terminal restoration", cases, filters)
}

/// A child on a PTY, with the raw output and a screen of what it drew.
/// The session is killed and reaped before the PTY closes.
struct Terminal {
    session: Session,
    pty: Pty,
    output: Vec<u8>,
    screen: Screen,
}

impl Terminal {
    fn master(&self) -> i32 {
        self.pty.master.as_raw_fd()
    }

    fn slave(&self) -> i32 {
        self.pty.slave.as_raw_fd()
    }

    fn write(&self, data: &[u8]) {
        pty::write_all(self.master(), data);
    }

    /// Reads until `marker` is in the raw output (a marker starting with ESC)
    /// or on the screen, failing after 10 seconds.
    fn until(&mut self, mode: &str, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let raw = marker.starts_with('\x1b');
        while !(if raw { contains(&self.output, marker.as_bytes()) } else { self.screen.text().contains(marker) }) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "{mode}: {}", String::from_utf8_lossy(&self.output));
            let ready = pty::readable(&[self.master()], remaining);
            assert!(!ready.is_empty(), "{mode}: TUI output deadline exceeded\n{}", self.screen.text());
            let data = pty::read_some(self.master());
            self.output.extend_from_slice(&data);
            self.screen.feed(&data);
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        self.pty.resize(rows, cols);
        pty::kill(self.session.id(), libc::SIGWINCH);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn tail(output: &[u8], length: usize) -> String {
    String::from_utf8_lossy(&output[output.len().saturating_sub(length)..]).into_owned()
}

/// The sorted names in HOME/.hamn.
fn hamn_entries(home: &Path) -> Vec<String> {
    let directory = home.join(".hamn");
    let entries = std::fs::read_dir(&directory).unwrap_or_else(|error| panic!("{}: {error}", directory.display()));
    let mut names: Vec<String> = entries.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// Only the TUI preferences (and their lock) exist: no profile state.
fn only_preferences(names: &[String]) -> bool {
    names == ["tui.json"] || names == ["tui.json", "tui.lock"]
}

fn exercise(mode: &str, command: Option<Command>) {
    let directory = TempDir::new_in(&std::env::temp_dir(), "hamn-tui-");
    let home = directory.path();
    let pty = Pty::open(24, 100);
    let before = termios::settings(pty.slave.as_raw_fd());
    let mut command = command.unwrap_or_else(|| Command::new(hamn()));
    command.env("HOME", home).env("TERM", "xterm-256color");
    let child = pty.spawn(&mut command);
    let mut terminal = Terminal { session: Session(child), pty, output: Vec::new(), screen: Screen::new(40, 160) };
    let terminal = &mut terminal;
    terminal.until(mode, "Hamn");
    assert!(contains(&terminal.output, b"\x1b[?1049h"));
    if mode != "panic" {
        terminal.write(b"1\r");
        terminal.until(mode, "[Containers]");
    }
    match mode {
        "q" => {
            terminal.write(b":contexts\r");
            terminal.until(mode, "k8s contexts list");
            terminal.write("/작업".as_bytes());
            terminal.until(mode, "작");
            terminal.until(mode, "업"); // incremental frames put CSI codes between characters
            terminal.write(b"\r");
            terminal.resize(10, 30);
            terminal.write(b"q");
        }
        "confirm-small" => {
            terminal.write(b":vm create --profile work\r");
            terminal.until(mode, "Impact:");
            terminal.resize(8, 30);
            terminal.until(mode, "disabled.");
            terminal.write(b"yn");
            terminal.until(mode, "selected"); // cancellation has returned to the small-screen warning
            terminal.resize(24, 100);
            terminal.write(b":contexts\r");
            terminal.until(mode, "k8s contexts list");
            assert!(only_preferences(&hamn_entries(home)), "hidden confirmation executed a mutation");
            terminal.write(b"q");
        }
        "interrupt" => terminal.write(b"\x03"),
        "suspend-key" | "suspend-signal" => {
            if mode == "suspend-key" {
                terminal.write(b"\x1a");
            } else {
                pty::kill(terminal.session.id(), libc::SIGTSTP);
            }
            terminal.until(mode, "\x1b[?1049l");
            assert_eq!(termios::settings(terminal.slave()), before);
            let status = wait_stopped(terminal.session.id(), Duration::from_secs(5))
                .expect("TUI did not stop after restoring the terminal");
            assert!(libc::WIFSTOPPED(status), "status {status:#x}");
            terminal.output.clear();
            pty::kill(terminal.session.id(), libc::SIGCONT);
            // The screen still holds the pre-suspend frame. Wait for the new
            // terminal entry, which follows raw-mode restoration on resume.
            terminal.until(mode, "\x1b[?1049h");
            assert_ne!(termios::settings(terminal.slave()), before);
            terminal.write(b"q");
        }
        "panic" => {} // the isolated Rust test intentionally unwinds after drawing
        _ => pty::kill(terminal.session.id(), libc::SIGTERM),
    }
    terminal.until(mode, "\x1b[?1049l");
    let status = exec::wait_timeout(&mut terminal.session.0, Duration::from_secs(5)).expect("exit within 5 s");
    assert_eq!(status.code(), Some(if mode == "panic" { 101 } else { 0 }), "{status:?}");
    let after = termios::settings(terminal.slave());
    assert_eq!(after, before, "terminal settings were not restored: {before:?} {after:?} {}", tail(&terminal.output, 3000));
    assert!(!home.join(".hamn").exists() || only_preferences(&hamn_entries(home)), "TUI observation changed profile state");
}

/// Waits up to `timeout` for the process `pid` to stop (or exit) and returns
/// its wait status. `waitpid(WUNTRACED)` has no timeout, so it runs on a
/// helper thread; a stopped child is reported, not reaped.
fn wait_stopped(pid: u32, timeout: Duration) -> Option<i32> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut status = 0;
        let result = loop {
            // SAFETY: status is a valid, writable int.
            if unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WUNTRACED) } >= 0 {
                break Ok(status);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                break Err(error);
            }
        };
        let _ = sender.send(result);
    });
    let result = receiver.recv_timeout(timeout).ok()?;
    Some(result.unwrap_or_else(|error| panic!("waitpid {pid}: {error}")))
}

/// The product's unit-test executable from `cargo test --no-run`, built the
/// way `make test-control-rust` builds it.
fn build_test_binary() -> PathBuf {
    let build = exec::output_within(
        Command::new("cargo").args(["test", "--locked", "--no-run", "--message-format=json"]),
        Duration::from_secs(180),
    );
    assert!(build.status.success(), "cargo test --no-run: {}", String::from_utf8_lossy(&build.stderr));
    let stdout = String::from_utf8(build.stdout).expect("cargo JSON is UTF-8");
    stdout
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str::<Value>(line).expect("cargo JSON message"))
        .find(|item| {
            item["reason"] == "compiler-artifact"
                && item["profile"]["test"].as_bool().unwrap_or(false)
                && item["executable"].as_str().is_some_and(|path| !path.is_empty())
        })
        .map(|item| PathBuf::from(item["executable"].as_str().unwrap()))
        .expect("a test executable")
}

/// A resize signal and a key that are ready in the same poll batch are both
/// delivered: the ignored test raises SIGWINCH, the key arrives, and only
/// then does a pipe gate let it poll.
fn coalesced_events(test_binary: &Path) {
    let pty = Pty::open(24, 100);
    let (gate_read, gate_write) = std::io::pipe().expect("pipe");
    let before = termios::settings(pty.slave.as_raw_fd());
    let gate = gate_read.as_raw_fd();
    let mut command = Command::new(test_binary);
    command
        .args(["tui::tests::resize_and_key_readiness_survive_the_same_poll_batch", "--ignored", "--exact", "--nocapture"])
        .env("HAMN_TEST_EVENT_GATE", gate.to_string());
    // SAFETY: fcntl is async-signal-safe; it makes only the gate's read end
    // inheritable (Python's pass_fds), at the same descriptor number.
    unsafe {
        command.pre_exec(move || match libc::fcntl(gate, libc::F_SETFD, 0) {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    let child = pty.spawn(&mut command);
    let mut terminal = Terminal { session: Session(child), pty, output: Vec::new(), screen: Screen::new(40, 160) };
    let until = |terminal: &mut Terminal, marker: &[u8]| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !contains(&terminal.output, marker) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero() && !pty::readable(&[terminal.master()], left).is_empty(),
                "{}",
                String::from_utf8_lossy(&terminal.output)
            );
            let data = pty::read_some(terminal.master());
            terminal.output.extend_from_slice(&data);
        }
    };
    until(&mut terminal, b"SIGNAL_READY");
    terminal.write(b"q");
    pty::write_all(gate_write.as_raw_fd(), b"\x01");
    until(&mut terminal, b"test result:");
    let status = exec::wait_timeout(&mut terminal.session.0, Duration::from_secs(5)).expect("exit within 5 s");
    assert!(status.success(), "{status:?} {}", String::from_utf8_lossy(&terminal.output));
    assert_eq!(termios::settings(terminal.slave()), before);
    drop(terminal);
    drop((gate_read, gate_write));
}
