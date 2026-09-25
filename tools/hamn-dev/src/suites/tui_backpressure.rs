//! Whole-TUI PTY backpressure checks; no VM, network or user configuration.
//! A 512 KiB bracketed paste queued for a raw CLI that does not read yet
//! keeps its bytes and order, and resize, output, frontend termination and
//! an explicit interrupt stay responsive meanwhile.
use crate::runner::{self, Case, case};
use crate::support::harness_peers::{GateOnDrop, select_peer, wait_gate};
use crate::support::pty;
use crate::support::tui::Harness;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PASTE_LENGTH: usize = 512 * 1024;
/// The ordered mode's tail after the paste.
const ORDERED_TAIL: &[u8] = b"END\x10\x11";

pub fn main(filters: &[String]) -> ExitCode {
    let cases: Vec<Case> = ["terminate", "interrupt", "ordered"]
        .into_iter()
        .map(|mode| case(format!("exercise/{mode}"), move || exercise(mode)))
        .collect();
    runner::run(
        "tui-backpressure",
        "queued paste preserves bytes and leaves resize, output, termination and explicit interrupt responsive",
        cases,
        filters,
    )
}

/// A detached fixture thread whose completion can be awaited with a bound
/// (Python's `join(timeout)` followed by `is_alive()`).
struct Worker {
    done: Receiver<()>,
}

impl Worker {
    fn spawn(work: impl FnOnce() + Send + 'static) -> Self {
        let (finished, done) = mpsc::channel();
        std::thread::spawn(move || {
            work();
            let _ = finished.send(());
        });
        Self { done }
    }

    /// Whether the thread has finished within `timeout`. A panicked thread
    /// drops its sender, which also counts as finished.
    fn joined(&self, timeout: Duration) -> bool {
        !matches!(self.done.recv_timeout(timeout), Err(RecvTimeoutError::Timeout))
    }
}

/// Writes all of `data`, retrying partial and interrupted writes.
fn write_fd(fd: &OwnedFd, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        // SAFETY: data is readable for its length.
        let count = unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        data = &data[count as usize..];
    }
    Ok(())
}

/// Sends `payload` to the frontend from another thread and then notifies the
/// harness's own bounded wait loop that the outer paste is fully sent.
fn deliver(harness: &Harness, payload: Vec<u8>, errors: Arc<Mutex<Vec<io::Error>>>) -> Worker {
    let master = harness.pty.master.try_clone().expect("duplicate PTY master");
    let notice = harness.notice.try_clone().expect("duplicate notice FIFO");
    Worker::spawn(move || {
        if let Err(error) = write_fd(&master, &payload).and_then(|()| write_fd(&notice, b"paste-delivered\n")) {
            errors.lock().unwrap().push(error);
        }
    })
}

/// Blocks until process `pid` (this process's child) has exited, without
/// reaping it, so that the harness still collects its status.
fn wait_exited_unreaped(pid: u32) {
    loop {
        // SAFETY: siginfo_t is plain data that waitid fills in.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waitid writes only `info`; WNOWAIT leaves the child waitable.
        let result = unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if result == 0 {
            return;
        }
        let error = io::Error::last_os_error();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted, "waitid {pid}: {error}");
    }
}

fn exercise(mode: &'static str) {
    let mut workers = Vec::new();
    {
        let mut harness = Harness::new("containers");
        // Declared after the harness: releases an old-binary reproduction
        // that remains blocked, before the harness closes.
        let _release = GateOnDrop::new(&harness.gate);
        run(&mut harness, mode, &mut workers);
    }
    for worker in &workers {
        assert!(worker.joined(Duration::from_secs(2)), "fixture thread was not cleaned up");
    }
}

fn run(harness: &mut Harness, mode: &str, workers: &mut Vec<Worker>) {
    harness.until("old-target-row");
    select_peer(&harness.root, "docker", "tui-backpressure");
    harness.send(format!(":stdin-proof {mode}\r").as_bytes(), "INPUT_READY");
    let tail: &[u8] = if mode == "ordered" { ORDERED_TAIL } else { b"\x03" };
    let paste = vec![b'x'; PASTE_LENGTH];
    let errors = Arc::new(Mutex::new(Vec::new()));
    let payload = [&b"\x1b[200~"[..], &paste, b"\x1b[201~", tail].concat();
    workers.push(deliver(harness, payload, errors.clone()));
    harness.noticed("paste-delivered");
    let sent = workers[0].joined(Duration::from_secs(1));
    assert!(sent && errors.lock().unwrap().is_empty(), "{:?}", errors.lock().unwrap());
    if mode == "ordered" {
        harness.release_gate();
        let digest = hex(&Sha256::digest([&paste[..], tail].concat()));
        harness.until(&format!("DIGEST:{digest}"));
        harness.until("Exit code 0");
    } else {
        // A raw CLI does not interpret Ctrl-C as a signal. The queued byte
        // must not become an unrequested out-of-band interrupt.
        harness.pty.resize(32, 150);
        pty::kill(harness.child.id(), libc::SIGWINCH);
        harness.until("RESIZED:150x29");
        assert!(!harness.text().contains("CLI_INTERRUPTED"), "{}", harness.text());
        if mode == "terminate" {
            let pid = harness.child.id();
            let notice = harness.notice.try_clone().expect("duplicate notice FIFO");
            workers.push(Worker::spawn(move || {
                // Python's child.wait(timeout=10): no notice after the deadline.
                let started = Instant::now();
                wait_exited_unreaped(pid);
                if started.elapsed() <= Duration::from_secs(10) {
                    write_fd(&notice, b"frontend-exited\n").expect("notice");
                }
            }));
            pty::kill(pid, libc::SIGTERM);
            harness.noticed("frontend-exited");
            let waited = workers[1].joined(Duration::from_secs(1));
            let status = harness.child.try_wait().expect("wait").expect("the frontend has exited");
            assert!(status.code() == Some(0) && waited, "{status:?}");
            return;
        }
        harness.send(b"\x1b\x03", "Exit code 130"); // Ctrl+Alt+C
        assert!(harness.text().contains("discarded"), "{}", harness.text());
    }
    harness.send(b"\r", "> abc123");
    assert!(harness.text().contains("old-target-row"), "{}", harness.text());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `docker`: lists one row, or for `stdin-proof` puts its terminal in raw
/// mode, reports signals, waits for the gate and then digests exactly the
/// ordered paste (or reports the release).
pub fn fixture(_program: &str, args: &[String]) -> ExitCode {
    let has = |value: &str| args.iter().any(|arg| arg == value);
    if !has("stdin-proof") {
        println!("{}", json!({"ID": "abc123", "Names": "old-target-row", "State": "running"}));
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    set_raw(0);
    install(libc::SIGINT, interrupted);
    install(libc::SIGTERM, interrupted);
    install(libc::SIGWINCH, resized);
    println!("INPUT_READY");
    wait_gate(&root);
    if has("ordered") {
        let expected = PASTE_LENGTH + ORDERED_TAIL.len();
        let mut data = Vec::with_capacity(expected);
        let mut buffer = vec![0u8; expected];
        while data.len() < expected {
            let wanted = expected - data.len();
            // SAFETY: buffer is writable for `wanted` <= its length bytes.
            let count = unsafe { libc::read(0, buffer.as_mut_ptr().cast(), wanted) };
            if count < 0 {
                let error = io::Error::last_os_error();
                assert_eq!(error.kind(), io::ErrorKind::Interrupted, "read stdin: {error}");
                continue;
            }
            if count == 0 {
                // End of input: the digest of the short data cannot match.
                break;
            }
            data.extend_from_slice(&buffer[..count as usize]);
        }
        println!("DIGEST:{}", hex(&Sha256::digest(&data)));
    } else {
        println!("RELEASED");
    }
    ExitCode::SUCCESS
}

/// Python's `tty.setraw(fd)`: its `cfmakeraw` flags, applied with TCSAFLUSH.
fn set_raw(fd: i32) {
    // SAFETY: termios is plain data that tcgetattr fills in.
    let mut mode: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: tcgetattr writes only `mode`.
    assert_eq!(unsafe { libc::tcgetattr(fd, &mut mode) }, 0, "tcgetattr: {}", io::Error::last_os_error());
    mode.c_iflag &= !(libc::IGNBRK
        | libc::BRKINT
        | libc::IGNPAR
        | libc::PARMRK
        | libc::INPCK
        | libc::ISTRIP
        | libc::INLCR
        | libc::IGNCR
        | libc::ICRNL
        | libc::IXON
        | libc::IXANY
        | libc::IXOFF);
    mode.c_oflag &= !libc::OPOST;
    mode.c_cflag &= !(libc::PARENB | libc::CSIZE);
    mode.c_cflag |= libc::CS8;
    mode.c_lflag &= !(libc::ECHO
        | libc::ECHOE
        | libc::ECHOK
        | libc::ECHONL
        | libc::ICANON
        | libc::IEXTEN
        | libc::ISIG
        | libc::NOFLSH
        | libc::TOSTOP);
    mode.c_cc[libc::VMIN] = 1;
    mode.c_cc[libc::VTIME] = 0;
    // SAFETY: tcsetattr reads only `mode`.
    assert_eq!(unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &mode) }, 0, "tcsetattr: {}", io::Error::last_os_error());
}

fn install(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    // SAFETY: sigaction is plain data; the handler only makes
    // async-signal-safe calls.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0, "{}", io::Error::last_os_error());
    }
}

/// SIGINT and SIGTERM: report the interrupt and exit at once. The handler
/// writes without Rust's stdout buffer, which the main flow may hold.
extern "C" fn interrupted(number: libc::c_int) {
    const MESSAGE: &[u8] = b"CLI_INTERRUPTED\n";
    // SAFETY: write and _exit are async-signal-safe.
    unsafe {
        libc::write(1, MESSAGE.as_ptr().cast(), MESSAGE.len());
        libc::_exit(128 + number);
    }
}

/// SIGWINCH: reports `RESIZED:COLUMNSxROWS` of the standard input terminal.
/// Formats into a stack buffer: no allocation in a signal handler.
extern "C" fn resized(_: libc::c_int) {
    // SAFETY: __error returns this thread's errno location; ioctl and write
    // are async-signal-safe, and errno is restored for the interrupted code.
    unsafe {
        let saved = *libc::__error();
        let mut size: libc::winsize = std::mem::zeroed();
        libc::ioctl(0, libc::TIOCGWINSZ, &mut size);
        let mut line = [0u8; 32];
        let mut length = 0;
        for part in [&b"RESIZED:"[..], &decimal(size.ws_col), b"x", &decimal(size.ws_row), b"\n"] {
            for &byte in part.iter().take_while(|&&byte| byte != 0) {
                line[length] = byte;
                length += 1;
            }
        }
        libc::write(1, line.as_ptr().cast(), length);
        *libc::__error() = saved;
    }
}

/// The decimal digits of `value`, NUL-padded.
fn decimal(mut value: u16) -> [u8; 5] {
    let mut digits = [0u8; 5];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    digits[..count].reverse();
    digits
}
