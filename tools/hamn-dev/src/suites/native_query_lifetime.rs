//! Structured CLI queries own their process group until completion or
//! cancellation: a descendant that holds only the write end of a private
//! FIFO must be gone (the FIFO reports end-of-file) after cancellation, a
//! regrouped parent, early exit, an error exit or an output overflow.
use super::tui_native_regressions::{self as native, record};
use crate::runner::{self, Case, case};
use crate::support::harness_peers::{
    GateOnDrop, fifo_reader, notify, read_up_to, select_peer, select_readable, wait_gate,
};
use crate::support::{pty, tui::Harness, tui::install_fixture};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

const MODES: [&str; 6] = ["cancel", "regroup", "exit", "error", "overflow", "stderr-overflow"];

pub fn main(filters: &[String]) -> ExitCode {
    let cases: Vec<Case> =
        MODES.into_iter().map(|mode| case(format!("exercise/{mode}"), move || exercise(mode))).collect();
    runner::run(
        "native-query-lifetime",
        "structured query cancellation, early exit and overflow clean owned children",
        cases,
        filters,
    )
}

fn exercise(mode: &str) {
    // Dropped in reverse order: the gate is released, the harness closes,
    // then the lifetime FIFO is closed (the Python suite's finally).
    let lifetime: OwnedFd;
    let mut harness = Harness::new("containers");
    let _release = GateOnDrop::new(&harness.gate);
    harness.until("old-target-row");
    lifetime = fifo_reader(&harness.root.join("lifetime"));
    install_fixture(&harness.root, "lifetime-child");
    select_peer(&harness.root, "lifetime-child", "native-query-lifetime-child");
    select_peer(&harness.root, "docker", "native-query-lifetime");
    harness.write(format!(":ps --filter lifetime-{mode}\r").as_bytes());
    harness.noticed(if mode == "regroup" { "parent-regrouped" } else { "child-ready" });
    assert_eq!(read_up_to(lifetime.as_raw_fd(), 1), b"R");
    if mode == "cancel" || mode == "regroup" {
        harness.send(b":version\r", "ACTION_DONE");
        harness.until("Exit code 0");
    } else {
        // Hold auto-refresh without cancelling the current query. A second
        // query must not open another writer while checking this one's EOF.
        if mode.ends_with("overflow") {
            harness.until("CLI output exceeds 16 MiB");
        } else if mode == "error" {
            harness.until("exit status: 7");
        }
        harness.send(b":LIFETIME_BARRIER", ":LIFETIME_BARRIER");
    }
    // Only the descendant holds the write end. EOF proves that cancellation,
    // parent exit, and output-limit failures close it, without PID polling.
    assert!(select_readable(lifetime.as_raw_fd(), Duration::from_secs(5)), "{mode}: query child survived");
    assert_eq!(read_up_to(lifetime.as_raw_fd(), 1), b"", "{mode}: unexpected descendant output");
    if mode == "exit" {
        harness.until("completed-query");
    }
    assert!(!contains(&harness.output, b"cannot reap query process"));
    assert!(!contains(&harness.output, b"cannot terminate query process group"));
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn fixture_root() -> PathBuf {
    PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"))
}

/// A pipe whose descriptors are closed on exec. Returns (read, write).
fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    // SAFETY: pipe writes two new descriptors into fds.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe: {}", io::Error::last_os_error());
    for fd in fds {
        // SAFETY: fd is a descriptor just created above.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
    }
    // SAFETY: both descriptors are new and owned by nobody else.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

/// The native `docker` peer, except that a query naming `lifetime-MODE`
/// starts `lifetime-child`, waits until it is ready and then behaves as MODE.
pub fn fixture(program: &str, args: &[String]) -> ExitCode {
    if !args.iter().any(|arg| arg.starts_with("lifetime-")) {
        return native::fixture(program, args);
    }
    let has = |value: &str| args.iter().any(|arg| arg == value);
    let root = fixture_root();
    record(&root, program, args);
    let (ready, notify_end) = pipe();
    let child_notify = notify_end.as_raw_fd();
    let mut command = Command::new(root.join("lifetime-child"));
    command.arg(child_notify.to_string());
    // SAFETY: fcntl is async-signal-safe; it lets only this child inherit
    // the notify end (Python's pass_fds).
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(child_notify, libc::F_SETFD, 0) == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
        });
    }
    let mut child = command.spawn().expect("spawn lifetime-child");
    drop(notify_end);
    assert_eq!(read_up_to(ready.as_raw_fd(), 1), b"1");
    drop(ready);
    if has("lifetime-regroup") {
        // SAFETY: getppid and getpgid only query; setpgid moves this process
        // into its parent's process group.
        let moved = unsafe { libc::setpgid(0, libc::getpgid(libc::getppid())) };
        assert_eq!(moved, 0, "setpgid: {}", io::Error::last_os_error());
        notify(&root, "parent-regrouped\n");
    }
    if has("lifetime-exit") {
        println!("{}", json!({"ID": "completed", "Names": "completed-query"}));
        io::stdout().flush().unwrap();
        std::process::exit(0);
    }
    if has("lifetime-error") {
        std::process::exit(7);
    }
    if has("lifetime-overflow") || has("lifetime-stderr-overflow") {
        let data = vec![b'x'; 17 * 1024 * 1024];
        if has("lifetime-stderr-overflow") {
            io::stderr().write_all(&data).expect("write stderr");
        } else {
            let mut stdout = io::stdout();
            stdout.write_all(&data).and_then(|()| stdout.flush()).expect("write stdout");
        }
    }
    child.wait().expect("wait lifetime-child");
    ExitCode::SUCCESS
}

/// The query's descendant: holds the lifetime FIFO's write end, reports
/// `child-ready`, signals its parent through the inherited descriptor and
/// waits for the gate.
pub fn child(_program: &str, args: &[String]) -> ExitCode {
    let root = fixture_root();
    let parent: i32 = args[0].parse().expect("notify descriptor");
    let mut lifetime = OpenOptions::new().write(true).open(root.join("lifetime")).unwrap();
    lifetime.write_all(b"R").unwrap();
    notify(&root, "child-ready\n");
    // SAFETY: the descriptor was inherited for this process to own.
    let parent = unsafe { OwnedFd::from_raw_fd(parent) };
    pty::write_all(parent.as_raw_fd(), b"1");
    drop(parent);
    wait_gate(&root);
    drop(lifetime);
    ExitCode::SUCCESS
}
