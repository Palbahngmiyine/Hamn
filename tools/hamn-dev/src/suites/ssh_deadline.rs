//! A real OpenSSH client must not wait forever on an unresponsive master:
//! build/tests/test_ssh_deadline drives each SSH operation against a
//! control socket that accepts and never answers, and a fresh-master start
//! shares its total deadline with an `ssh` that never authenticates. The
//! exec case also runs with the delays of a loaded machine: a master that
//! accepts only after the client's deadline, and an `ssh` slow to start.
use crate::runner::{self, Case, case};
use crate::support::bounded_process;
use crate::support::pty;
use crate::support::tmp::TempDir;
use crate::support::tui::install_fixture;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The fixture that stands in for `ssh` in the fresh-master case.
pub const UNRESPONSIVE_SSH: &str = "ssh-deadline-unresponsive";

/// The fixture that stands in for `ssh` in the slow-start case: it waits
/// `SLOW_SSH_START`, then runs `/usr/bin/ssh`.
pub const SLOW_START_SSH: &str = "ssh-deadline-slow-start";

/// How long the slow-start `ssh` takes before it can connect.
const SLOW_SSH_START: Duration = Duration::from_millis(300);

/// What a case adds to an unresponsive master: the delays a loaded machine
/// can put between the client's start, its connection and the master's
/// accept.
#[derive(Clone, Copy, Debug)]
enum Hazard {
    None,
    /// The master accepts this long after the connection arrives.
    LateAccept(Duration),
    /// `ssh` takes `SLOW_SSH_START` to start before it connects.
    SlowSshStart,
}

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = Vec::new();
    // The exec deadline (tests/host/test_ssh_deadline.c) also covers starting
    // ssh. At 200 ms a start that a loaded machine delayed left no time to
    // reach the master, and the case no longer tested an unresponsive one.
    for (operation, budget) in [("alive", 5.0), ("exit", 5.0), ("start", 1.0), ("forward", 5.0), ("cancel", 5.0), ("exec", 1.0)] {
        cases.push(case(format!("unresponsive_master/{operation}"), move || {
            unresponsive_master(operation, Duration::from_secs_f64(budget), Hazard::None)
        }));
    }
    let exec = Duration::from_secs(1);
    cases.push(case("unresponsive_master/exec/late_accept", move || {
        unresponsive_master("exec", exec, Hazard::LateAccept(exec + Duration::from_millis(500)))
    }));
    cases.push(case("unresponsive_master/exec/slow_ssh_start", move || {
        unresponsive_master("exec", exec, Hazard::SlowSshStart)
    }));
    cases.push(case("fresh_master_start_shares_the_total_deadline", fresh_master_start_shares_the_total_deadline));
    for operation in ["cancel", "forward"] {
        cases.push(case(format!("absent_master/{operation}_is_quiet"), move || absent_master_request_is_quiet(operation)));
    }
    runner::run("ssh-deadline", "real SSH check/exit/start/forward/cancel/exec obey operation deadlines", cases, filters)
}

/// `$SSH_DEADLINE_TEST`, or build/tests/test_ssh_deadline.
fn binary() -> PathBuf {
    let path =
        std::env::var_os("SSH_DEADLINE_TEST").map_or_else(|| PathBuf::from("build/tests/test_ssh_deadline"), PathBuf::from);
    std::fs::canonicalize(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn unresponsive_master(operation: &str, budget: Duration, hazard: Hazard) {
    let binary = binary();
    let directory = TempDir::new("hamn-ssh-");
    let accept_delay = match hazard {
        Hazard::LateAccept(delay) => delay,
        Hazard::None | Hazard::SlowSshStart => Duration::ZERO,
    };
    let server = SilentMaster::start(&directory.path().join("ssh.sock"), accept_delay);
    let mut command = Command::new(&binary);
    command.arg(directory.path()).arg(operation).env("PATH", "/usr/bin:/bin");
    if let Hazard::SlowSshStart = hazard {
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        install_fixture(&bin, "ssh");
        command
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HAMN_DEV_FIXTURE", SLOW_START_SSH)
            .env_remove("FIXTURE_ROOT");
    }
    let started = Instant::now();
    let result = bounded_process::output(&mut command, budget + Duration::from_secs(4));
    let elapsed = started.elapsed();
    assert!(
        server.accepted_within(Duration::from_secs(5)),
        "{operation}: the client ended without reaching the master: {result:?}"
    );
    assert_eq!(result.status.code(), Some(0), "{operation}: {result:?}");
    assert!(budget <= elapsed && elapsed < budget + Duration::from_secs(4), "{operation}: {elapsed:?}");
    if operation == "forward" {
        let text = String::from_utf8_lossy(&result.stdout);
        assert_eq!(text.matches("completion observed failure").count(), 1, "{result:?}");
    }
    server.finish();
}

/// Callers of a TCP forward or a cancel request report its failure
/// themselves: a cancel after the master ended with its guest is expected,
/// and a forward the port observer cannot create is retried twice a second.
/// The real client's complaint about the missing control socket must not
/// reach stderr; it stays available to the caller as the request's detail.
fn absent_master_request_is_quiet(operation: &str) {
    let binary = binary();
    let directory = TempDir::new("hamn-ssh-");
    let result = bounded_process::output(
        Command::new(&binary).arg(directory.path()).arg(operation).env("PATH", "/usr/bin:/bin"),
        Duration::from_secs(9),
    );
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    let detail = String::from_utf8_lossy(&result.stdout);
    let prefix = format!("{operation} detail: ");
    assert!(detail.lines().any(|line| line.starts_with(&prefix) && line.ends_with("ssh.sock): No such file or directory")), "{result:?}");
}

/// A fresh-master attempt must share the total deadline, including
/// authentication.
fn fresh_master_start_shares_the_total_deadline() {
    let binary = binary();
    let directory = TempDir::new("hamn-ssh-start-");
    install_fixture(directory.path(), "ssh");
    let started = Instant::now();
    let result = bounded_process::output(
        Command::new(&binary)
            .arg(directory.path())
            .arg("start")
            .env("PATH", format!("{}:/usr/bin:/bin", directory.path().display()))
            .env("HAMN_DEV_FIXTURE", UNRESPONSIVE_SSH)
            .env_remove("FIXTURE_ROOT"),
        Duration::from_secs(5),
    );
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    let elapsed = started.elapsed();
    assert!(Duration::from_secs(1) <= elapsed && elapsed < Duration::from_secs(5), "{elapsed:?}");
}

/// `ssh` whose control commands (`-O ...`) fail and whose master never
/// authenticates: it waits for signals until it is killed.
pub fn unresponsive_ssh(_program: &str, args: &[String]) -> ExitCode {
    if args.iter().any(|arg| arg == "-O") {
        return ExitCode::from(255);
    }
    loop {
        // SAFETY: pause only suspends this thread until a signal arrives.
        unsafe { libc::pause() };
    }
}

/// `ssh` that takes `SLOW_SSH_START` to start, as a loaded machine can make
/// it, then becomes `/usr/bin/ssh` with the same arguments and process.
pub fn slow_start_ssh(_program: &str, args: &[String]) -> ExitCode {
    std::thread::sleep(SLOW_SSH_START);
    crate::support::real_cli::exec(Command::new("/usr/bin/ssh").args(args))
}

/// An SSH control socket that accepts one connection and holds it, without
/// a byte, until released (at most 12 seconds). No connection within 10
/// seconds is an error.
struct SilentMaster {
    /// Sent once the connection is accepted.
    accepted: mpsc::Receiver<()>,
    errors: Arc<Mutex<Vec<String>>>,
    release: mpsc::Sender<()>,
    done: mpsc::Receiver<()>,
}

impl SilentMaster {
    /// Listens at `path` and accepts `accept_delay` after a connection
    /// arrives.
    fn start(path: &Path, accept_delay: Duration) -> Self {
        let listener = UnixListener::bind(path).unwrap_or_else(|error| panic!("bind {}: {error}", path.display()));
        // The Python fixture listened with a backlog of one; std uses 128.
        // SAFETY: listen on a descriptor this listener owns only changes
        // its backlog.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0, "listen: {}", std::io::Error::last_os_error());
        let (accept, accepted) = mpsc::channel();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = mpsc::channel::<()>();
        let (finished, done) = mpsc::channel();
        let failures = Arc::clone(&errors);
        std::thread::spawn(move || {
            let result = if pty::readable(&[listener.as_raw_fd()], Duration::from_secs(10)).is_empty() {
                Err("no connection within 10 seconds".to_owned())
            } else {
                std::thread::sleep(accept_delay);
                listener.accept().map_err(|error| format!("accept: {error}")).and_then(|(connection, _)| {
                    let _ = accept.send(());
                    let outcome = match released.recv_timeout(Duration::from_secs(12)) {
                        Err(RecvTimeoutError::Timeout) => Err("not released within 12 seconds".to_owned()),
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => Ok(()),
                    };
                    drop(connection);
                    outcome
                })
            };
            if let Err(error) = result {
                failures.lock().unwrap().push(error);
            }
            drop(listener);
            let _ = finished.send(());
        });
        Self { accepted, errors, release, done }
    }

    /// Whether the master accepts a connection within `timeout`. A client
    /// that connected waits in the listener's backlog until the master's
    /// thread accepts it, which can happen after the client has ended, so
    /// the moment the client is done is too early to look.
    fn accepted_within(&self, timeout: Duration) -> bool {
        self.accepted.recv_timeout(timeout).is_ok()
    }

    /// Releases the held connection and requires the server to finish
    /// within 12 seconds without an error. (A panicking case drops the
    /// sender instead, which also releases it.)
    fn finish(self) {
        let _ = self.release.send(());
        let alive = self.done.recv_timeout(Duration::from_secs(12)).is_err();
        let errors = self.errors.lock().unwrap().clone();
        assert!(errors.is_empty() && !alive, "server errors {errors:?}, still running: {alive}");
    }
}
