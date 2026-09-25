//! `vm stop` in the TUI stays interactive and completes while the profile's
//! SSH control socket accepts a connection and never answers.
use crate::runner::{self, case};
use crate::support::exec::{self, Session};
use crate::support::pty::{self, Pty};
use crate::support::screen::Screen;
use crate::support::termios;
use crate::support::{hamn, tmp::TempDir};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "tui-ssh-timeout",
        "TUI remains interactive and stops with a stalled SSH control socket",
        vec![case("vm_stop_with_stalled_ssh_socket", vm_stop_with_stalled_ssh_socket)],
        filters,
    )
}

/// Python's `\s` on bytes: space, \t, \n, \r, \f and \v.
fn without_whitespace(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c' | '\x0b')).collect()
}

/// The fixture SSH server: it accepts one connection within 10 seconds,
/// marks it accepted and reports it on the notice pipe (`1`, or `0` after
/// an error), and holds it unanswered until released (at most 15 seconds).
struct StalledServer {
    release: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    accepted: Arc<AtomicBool>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl StalledServer {
    fn start(listener: UnixListener, notice: std::io::PipeWriter) -> Self {
        let (release, released) = mpsc::channel::<()>();
        let errors: Arc<Mutex<Vec<String>>> = Arc::default();
        let recorded = Arc::clone(&errors);
        let accepted: Arc<AtomicBool> = Arc::default();
        let marked = Arc::clone(&accepted);
        let thread = std::thread::spawn(move || {
            let mut notice = notice;
            let result = (|| {
                listener.set_nonblocking(true).map_err(|error| error.to_string())?;
                if pty::readable(&[listener.as_raw_fd()], Duration::from_secs(10)).is_empty() {
                    return Err("accept timed out".to_owned());
                }
                let (connection, _) = listener.accept().map_err(|error| error.to_string())?;
                marked.store(true, Ordering::SeqCst);
                notice.write_all(b"1").map_err(|error| error.to_string())?;
                let outcome = match released.recv_timeout(Duration::from_secs(15)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => Err("not released within 15 s".to_owned()),
                    _ => Ok(()),
                };
                drop(connection);
                outcome
            })();
            if let Err(error) = result {
                recorded.lock().unwrap().push(error);
                let _ = notice.write_all(b"0");
            }
        });
        Self { release: Some(release), thread: Some(thread), accepted, errors }
    }

    fn accepted(&self) -> bool {
        self.accepted.load(Ordering::SeqCst)
    }

    fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }

    /// Releases the held connection and joins the thread, which every wait
    /// above bounds.
    fn stop(&mut self) {
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            thread.join().expect("server thread");
        }
    }
}

impl Drop for StalledServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn vm_stop_with_stalled_ssh_socket() {
    let directory = TempDir::new("hamn-tui-ssh-");
    let home = directory.path();
    let environment = |command: &mut Command| {
        command.env("HOME", home).env("PATH", "/usr/bin:/bin").env("TERM", "xterm-256color");
    };
    let mut create = Command::new(hamn());
    create.args(["--headless", "vm", "create", "--profile", "test", "--yes"]);
    environment(&mut create);
    let created = exec::output_within(&mut create, Duration::from_secs(10));
    assert!(created.status.success(), "{created:?}");
    let created: Value = serde_json::from_slice(&created.stdout).unwrap();
    assert!(created["ok"].as_bool().unwrap_or(false), "{created}");
    let socket = home.join(".hamn/test/ssh.sock");
    let listener = UnixListener::bind(&socket).unwrap_or_else(|error| panic!("{}: {error}", socket.display()));
    // Python's listen(1): std listens with a backlog of 128, and a second
    // connection attempt must not queue where it would have been refused.
    // SAFETY: listen on a listening socket only changes its backlog.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0, "listen: {}", std::io::Error::last_os_error());
    // The test holds the notice pipe's write end to the end, as before.
    let (mut notice_read, notice_write) = std::io::pipe().expect("pipe");
    let mut server = StalledServer::start(listener, notice_write.try_clone().expect("pipe"));
    let pty = Pty::open(36, 140);
    let original = termios::settings(pty.slave.as_raw_fd());
    let mut command = Command::new(hamn());
    environment(&mut command);
    let mut session = Session(pty.spawn(&mut command));
    let master = pty.master.as_raw_fd();
    let mut output = Vec::new();
    let mut screen = Screen::new(40, 160);
    let until = |output: &mut Vec<u8>, screen: &mut Screen, marker: &str, timeout: Duration| {
        let deadline = Instant::now() + timeout;
        while !without_whitespace(&screen.text()).contains(marker) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || pty::readable(&[master], remaining).is_empty() {
                panic!("{marker:?} {}", String::from_utf8_lossy(&output[output.len().saturating_sub(2000)..]));
            }
            let data = pty::read_some(master);
            output.extend_from_slice(&data);
            screen.feed(&data);
        }
    };
    let ten = Duration::from_secs(10);
    until(&mut output, &mut screen, "Hamn", ten);
    pty::write_all(master, b"1\r");
    until(&mut output, &mut screen, "[Containers]", ten);
    pty::write_all(master, b":vm stop --profile test\r");
    until(&mut output, &mut screen, "Impact:", ten);
    output.clear();
    pty::write_all(master, b"y");
    // Continue draining redraws: waiting only on the server can fill the PTY
    // and prevent the frontend from consuming confirmation or starting work.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server.accepted() {
        let tail = |output: &[u8]| String::from_utf8_lossy(&output[output.len().saturating_sub(4000)..]).into_owned();
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "VM stop did not attempt SSH cleanup {:?} {}", server.errors(), tail(&output));
        let ready = pty::readable(&[master, notice_read.as_raw_fd()], remaining);
        if ready.contains(&master) {
            let data = pty::read_some(master);
            output.extend_from_slice(&data);
            screen.feed(&data);
        }
        if ready.contains(&notice_read.as_raw_fd()) {
            let mut byte = [0u8];
            let count = notice_read.read(&mut byte).unwrap();
            assert_eq!(&byte[..count], b"1", "{:?} {}", server.errors(), tail(&output));
        }
    }
    // Input must remain responsive while the C worker waits on SSH.
    pty::write_all(master, b"?");
    until(&mut output, &mut screen, "Commands:", Duration::from_secs(2));
    until(&mut output, &mut screen, "Operationcompleted", ten);
    // A background completion must not replace the help/navigation screen.
    let mut status = Command::new(hamn());
    status.args(["--headless", "vm", "status", "--profile", "test"]);
    environment(&mut status);
    let status = exec::output_within(&mut status, ten);
    assert!(status.status.success(), "{status:?}");
    let snapshot = serde_json::from_slice::<Value>(&status.stdout).unwrap()["data"].clone();
    assert_eq!(snapshot["state"], "stopped", "{snapshot}");
    assert_eq!(snapshot["lastOperation"]["status"], "completed", "{snapshot}");
    pty::write_all(master, b"q");
    let exit = exec::wait_timeout(&mut session.0, Duration::from_secs(5)).expect("exit within 5 s");
    assert_eq!(exit.code(), Some(0), "{exit:?}");
    assert_eq!(termios::settings(pty.slave.as_raw_fd()), original);
    assert!(!socket.exists());
    server.stop();
    let errors = server.errors();
    assert!(errors.is_empty(), "{errors:?}");
    drop(notice_write);
}
