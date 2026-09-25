//! The UDP relay (`BINARY udp-forward ...`, from tests/host/test_port_forwarding.c
//! or a production `hamn`) recovers from injected send, receive and poll
//! failures and isolates its 64-flow boundary: a 65th client evicts exactly
//! the oldest flow.
//!
//! `hamn-dev test udp-proxy BINARY WORK_DIRECTORY [--production] [FILTER...]`
//! With `--production` only the flow boundary runs, since a production
//! binary has no fault injection.
use crate::runner::{self, Case, case};
use crate::support::bounded_process::{self, Captured};
use crate::support::pty;
use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const FAULT_VARIABLES: [&str; 3] = ["HAMN_TEST_UDP_SEND_FAILURE", "HAMN_TEST_UDP_RECV_FAILURE", "HAMN_TEST_UDP_POLL_FAILURE"];
const FLOW_LIMIT: usize = 64;
const TIMEOUT: Duration = Duration::from_secs(5);

/// A case body, given the relay executable and the work directory.
type Test = fn(&Path, &Path);

pub fn main(args: &[String]) -> ExitCode {
    let usage = "usage: hamn-dev test udp-proxy TEST_BINARY WORK_DIRECTORY [--production] [FILTER...]";
    let [binary, work, rest @ ..] = args else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };
    let production = rest.first().is_some_and(|arg| arg == "--production");
    let filters = if production { &rest[1..] } else { rest };
    let (binary, work) = (PathBuf::from(binary), PathBuf::from(work));
    // Like Python's mkdir(parents=True): parents may exist, the directory not.
    if let Err(error) = work.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::create_dir(&work)) {
        eprintln!("udp-proxy: {}: {error}", work.display());
        return ExitCode::FAILURE;
    }
    let mut cases: Vec<Case> = Vec::new();
    let tests: [(&str, Test); 4] = [
        ("send_recovery", send_recovery),
        ("recv_recovery", recv_recovery),
        ("poll_failure", poll_failure),
        ("flow_limit_and_eviction", flow_limit_and_eviction),
    ];
    for (name, test) in tests {
        if production && name != "flow_limit_and_eviction" {
            continue;
        }
        let (binary, work) = (binary.clone(), work.clone());
        cases.push(case(name, move || test(&binary, &work)));
    }
    let summary = if production {
        "production UDP relay isolates its 64-flow boundary"
    } else {
        "UDP relay recovers faults and isolates its 64-flow boundary"
    };
    runner::run("udp-proxy", summary, cases, filters)
}

fn fail(message: String) -> ! {
    panic!("FAIL: {message}");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn text(bytes: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(bytes))
}

/// The inherited environment without fault injection, plus `fault=1`.
fn fault_environment(command: &mut Command, fault: Option<&str>) {
    for variable in FAULT_VARIABLES {
        command.env_remove(variable);
    }
    if let Some(fault) = fault {
        command.env(fault, "1");
    }
}

/// Starts `binary udp-forward` on an inherited listener bound to an
/// ephemeral loopback port, relaying to `target_port` (`None`: the listening
/// port itself), with piped stdout and stderr. `pidfile` names the pidfile
/// for the listening port.
fn spawn_forwarder(
    binary: &Path,
    target_port: Option<u16>,
    pidfile: impl FnOnce(u16) -> PathBuf,
    fault: Option<&str>,
) -> (Child, u16, PathBuf) {
    let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    let listen_port = listener.local_addr().unwrap().port();
    let target_port = target_port.unwrap_or(listen_port);
    let fd = listener.as_raw_fd();
    let pidfile = pidfile(listen_port);
    let mut command = Command::new(binary);
    command
        .arg("udp-forward")
        .args(["--listen-address", "127.0.0.1", "--listen-port", &listen_port.to_string(), "--listen-fd", &fd.to_string()])
        .args(["--target-address", "127.0.0.1", "--target-port", &target_port.to_string(), "--pidfile"])
        .arg(&pidfile)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    fault_environment(&mut command, fault);
    // SAFETY: runs in the child between fork and exec and only calls fcntl,
    // which is async-signal-safe, to let the listener survive exec (Python's
    // pass_fds). The parent's descriptor keeps close-on-exec.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap_or_else(|error| panic!("spawn {}: {error}", binary.display()));
    drop(listener);
    (child, listen_port, pidfile)
}

/// A running relay. `stop` performs the checked shutdown; dropping a relay
/// that was not stopped (a failing case) only kills and reaps it.
struct Relay {
    child: Child,
    listen_port: u16,
    pidfile: PathBuf,
}

impl Relay {
    fn start(binary: &Path, work: &Path, target_port: u16, fault: Option<&str>) -> Self {
        let (child, listen_port, pidfile) =
            spawn_forwarder(binary, Some(target_port), |port| work.join(format!("relay-{port}.pid")), fault);
        let mut relay = Self { child, listen_port, pidfile };
        relay.wait_for_pidfile();
        relay
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().expect("poll relay").is_none()
    }

    fn signal(&self, signal: i32) {
        pty::kill(self.child.id(), signal);
    }

    fn wait_for_pidfile(&mut self) {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if self.pidfile.is_file() {
                return;
            }
            if !self.running() {
                let mut captured = Captured::default();
                let status = bounded_process::communicate(&mut self.child, &mut captured, TIMEOUT);
                fail(format!(
                    "UDP relay exited before readiness: rc={status:?}, stdout={}, stderr={}",
                    text(&captured.stdout),
                    text(&captured.stderr)
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        fail("UDP relay did not publish its pidfile within 5s".to_owned());
    }

    /// Resumes a stopped relay, asks it to stop with SIGTERM, and requires a
    /// zero exit within 5 seconds that removed the pidfile.
    fn stop(mut self) {
        if self.running() {
            self.signal(libc::SIGCONT);
        }
        if self.running() {
            self.signal(libc::SIGTERM);
        }
        let mut captured = Captured::default();
        let Some(status) = bounded_process::communicate(&mut self.child, &mut captured, TIMEOUT) else {
            let _ = self.child.kill();
            bounded_process::communicate(&mut self.child, &mut captured, TIMEOUT);
            fail(format!(
                "UDP relay did not stop within 5s: stdout={}, stderr={}",
                text(&captured.stdout),
                text(&captured.stderr)
            ));
        };
        if status.code() != Some(0) {
            fail(format!(
                "UDP relay shutdown failed: rc={status:?}, stdout={}, stderr={}",
                text(&captured.stdout),
                text(&captured.stderr)
            ));
        }
        if self.pidfile.exists() {
            fail("UDP relay pidfile remained after shutdown".to_owned());
        }
    }

    /// Reads the relay's stderr until it contains `expected`, within 5
    /// seconds.
    fn wait_for_stderr(&mut self, expected: &[u8]) {
        let deadline = Instant::now() + TIMEOUT;
        let mut output = Vec::new();
        let mut chunk = vec![0u8; 4096];
        while !contains(&output, expected) {
            let stderr = self.child.stderr.as_mut().expect("relay stderr");
            let remaining = deadline.checked_duration_since(Instant::now()).filter(|remaining| !remaining.is_zero());
            let Some(remaining) = remaining else {
                fail(format!("UDP relay did not report {}: {}", text(expected), text(&output)));
            };
            if pty::readable(&[stderr.as_raw_fd()], remaining).is_empty() {
                fail(format!("UDP relay did not report {}: {}", text(expected), text(&output)));
            }
            let count = stderr.read(&mut chunk).expect("read relay stderr");
            if count == 0 {
                let status = self.child.try_wait().expect("poll relay");
                fail(format!("UDP relay exited before reporting {}: rc={status:?}, stderr={}", text(expected), text(&output)));
            }
            output.extend_from_slice(&chunk[..count]);
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if self.running() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A loopback UDP peer that echoes `expected` datagrams on its own thread.
struct EchoTarget {
    port: u16,
    done: mpsc::Receiver<Result<(), String>>,
}

impl EchoTarget {
    fn start(expected: usize) -> Self {
        let target = UdpSocket::bind("127.0.0.1:0").unwrap();
        target.set_read_timeout(Some(TIMEOUT)).unwrap();
        target.set_write_timeout(Some(TIMEOUT)).unwrap();
        let port = target.local_addr().unwrap().port();
        let (finished, done) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (0..expected).try_for_each(|_| {
                let mut buffer = [0u8; 1024];
                let (count, address) = target.recv_from(&mut buffer).map_err(|error| format!("recvfrom: {error}"))?;
                target.send_to(&buffer[..count], address).map(drop).map_err(|error| format!("sendto: {error}"))
            });
            let _ = finished.send(result);
        });
        Self { port, done }
    }

    /// Requires the echo thread to have finished without an error within 5
    /// seconds.
    fn finish(self) {
        match self.done.recv_timeout(TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => fail(format!("UDP echo target did not finish cleanly: [{error}]")),
            Err(_) => fail("UDP echo target did not finish cleanly: still running".to_owned()),
        }
    }
}

/// A client socket as Python's unbound `socket()` would use it: the first
/// send binds it to the wildcard address.
fn client() -> UdpSocket {
    let client = UdpSocket::bind("0.0.0.0:0").unwrap();
    client.set_read_timeout(Some(TIMEOUT)).unwrap();
    client.set_write_timeout(Some(TIMEOUT)).unwrap();
    client
}

fn exchange(client: &UdpSocket, listen_port: u16, payload: &[u8]) {
    client.send_to(payload, ("127.0.0.1", listen_port)).unwrap();
    let mut buffer = [0u8; 1024];
    let (count, _) = client.recv_from(&mut buffer).unwrap_or_else(|error| panic!("recvfrom relay: {error}"));
    if &buffer[..count] != payload {
        fail(format!("UDP relay changed payload: {} != {}", text(&buffer[..count]), text(payload)));
    }
}

fn send_recovery(binary: &Path, work: &Path) {
    let target = EchoTarget::start(1);
    let mut relay = Relay::start(binary, work, target.port, Some("HAMN_TEST_UDP_SEND_FAILURE"));
    let client = client();
    client.send_to(b"injected-send-failure", ("127.0.0.1", relay.listen_port)).unwrap();
    relay.wait_for_stderr(b"UDP relay cannot send to target");
    exchange(&client, relay.listen_port, b"send-recovered");
    drop(client);
    relay.stop();
    target.finish();
}

fn recv_recovery(binary: &Path, work: &Path) {
    let target = EchoTarget::start(2);
    let mut relay = Relay::start(binary, work, target.port, Some("HAMN_TEST_UDP_RECV_FAILURE"));
    let client = client();
    client.send_to(b"injected-recv-failure", ("127.0.0.1", relay.listen_port)).unwrap();
    relay.wait_for_stderr(b"UDP relay cannot receive from target");
    exchange(&client, relay.listen_port, b"recv-recovered");
    drop(client);
    relay.stop();
    target.finish();
}

fn poll_failure(binary: &Path, work: &Path) {
    // The relay targets its own listening port; it must fail before relaying.
    let pidfile = work.join("poll-failure.pid");
    let (child, listen_port, _) = spawn_forwarder(binary, None, |_| pidfile.clone(), Some("HAMN_TEST_UDP_POLL_FAILURE"));
    // Held as a Relay only so a failing check still kills and reaps it.
    let mut relay = Relay { child, listen_port, pidfile: pidfile.clone() };
    let mut captured = Captured::default();
    let status = bounded_process::communicate(&mut relay.child, &mut captured, TIMEOUT);
    let status = status.unwrap_or_else(|| panic!("injected poll failure: relay still running after 5s"));
    if status.code() == Some(0) || !contains(&captured.stderr, b"UDP relay poll failed") {
        fail(format!("injected poll failure was hidden: rc={status:?}"));
    }
    if pidfile.exists() {
        fail("poll failure left a UDP relay pidfile".to_owned());
    }
    drop(relay);

    let target = EchoTarget::start(1);
    let relay = Relay::start(binary, work, target.port, None);
    let client = client();
    exchange(&client, relay.listen_port, b"poll-recovered");
    drop(client);
    relay.stop();
    target.finish();
}

fn flow_limit_and_eviction(binary: &Path, work: &Path) {
    let target = UdpSocket::bind("127.0.0.1:0").unwrap();
    target.set_read_timeout(Some(TIMEOUT)).unwrap();
    target.set_write_timeout(Some(TIMEOUT)).unwrap();
    let relay = Relay::start(binary, work, target.local_addr().unwrap().port(), None);
    let listen = ("127.0.0.1", relay.listen_port);
    let mut clients = Vec::new();
    let mut target_addresses: Vec<SocketAddr> = Vec::new();
    let mut buffer = [0u8; 1024];
    for index in 0..=FLOW_LIMIT {
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_nonblocking(true).unwrap();
        let payload = format!("open-{index}");
        client.send_to(payload.as_bytes(), listen).unwrap();
        clients.push(client);
        let (count, target_address) = target.recv_from(&mut buffer).unwrap_or_else(|error| panic!("flow {index}: {error}"));
        if &buffer[..count] != payload.as_bytes() {
            fail(format!("flow {index} opened with another payload: {}", text(&buffer[..count])));
        }
        target_addresses.push(target_address);
    }

    // Queue every reply while the relay is stopped, so it sees them all at
    // once when it resumes.
    relay.signal(libc::SIGSTOP);
    let pid = relay.child.id() as libc::pid_t;
    let mut status = 0;
    let stopped_pid = loop {
        // SAFETY: waitpid writes one status integer; WUNTRACED reports the
        // stop without reaping the child, which Child still owns.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        if result >= 0 || std::io::Error::last_os_error().kind() != ErrorKind::Interrupted {
            break result;
        }
    };
    if stopped_pid != pid || !libc::WIFSTOPPED(status) {
        fail("UDP relay did not enter the deterministic test barrier".to_owned());
    }
    for (index, target_address) in target_addresses.iter().enumerate() {
        target.send_to(format!("reply-{index}").as_bytes(), target_address).unwrap();
    }

    relay.signal(libc::SIGCONT);
    clients[FLOW_LIMIT].send_to(b"barrier", listen).unwrap();
    let (count, barrier_address) = target.recv_from(&mut buffer).unwrap_or_else(|error| panic!("barrier: {error}"));
    if &buffer[..count] != b"barrier" {
        fail(format!("flow barrier received another payload: {}", text(&buffer[..count])));
    }
    target.send_to(b"barrier-complete", barrier_address).unwrap();

    let stdout = relay.child.stdout.as_ref().expect("relay stdout").as_raw_fd();
    let mut fds = vec![stdout];
    fds.extend(clients.iter().map(AsRawFd::as_raw_fd));
    let mut responses: Vec<Vec<Vec<u8>>> = vec![Vec::new(); clients.len()];
    let deadline = Instant::now() + TIMEOUT;
    let mut barrier_seen = false;
    while !barrier_seen {
        let remaining = deadline.checked_duration_since(Instant::now()).filter(|remaining| !remaining.is_zero());
        let Some(remaining) = remaining else { fail("UDP flow boundary barrier timed out".to_owned()) };
        let ready = pty::readable(&fds, remaining);
        if ready.is_empty() {
            fail("UDP flow boundary barrier timed out".to_owned());
        }
        for fd in ready {
            if fd == stdout {
                fail("UDP relay exited during flow boundary test".to_owned());
            }
            let index = clients.iter().position(|client| client.as_raw_fd() == fd).expect("a client socket");
            loop {
                match clients[index].recv_from(&mut buffer) {
                    Ok((count, _)) => {
                        responses[index].push(buffer[..count].to_vec());
                        if &buffer[..count] == b"barrier-complete" {
                            barrier_seen = true;
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => panic!("recvfrom client {index}: {error}"),
                }
            }
        }
    }
    for fd in pty::readable(&fds, Duration::ZERO) {
        if let Some(index) = clients.iter().position(|client| client.as_raw_fd() == fd) {
            match clients[index].recv_from(&mut buffer) {
                Ok((count, _)) => responses[index].push(buffer[..count].to_vec()),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("recvfrom client {index}: {error}"),
            }
        }
    }

    let mut live_old_flows = 0;
    for (index, response) in responses.iter().enumerate() {
        let expected = format!("reply-{index}").into_bytes();
        if index == FLOW_LIMIT {
            if *response != [expected, b"barrier-complete".to_vec()] {
                fail(format!("new flow received mixed responses: {response:?}"));
            }
        } else if *response == [expected] {
            live_old_flows += 1;
        } else if !response.is_empty() {
            fail(format!("existing flow {index} received mixed responses: {response:?}"));
        }
    }
    if live_old_flows != FLOW_LIMIT - 1 {
        fail(format!("65th flow left {live_old_flows} existing flows active; expected {}", FLOW_LIMIT - 1));
    }
    relay.stop();
}
