//! A real OpenSSH client must not wait forever on an unresponsive master:
//! build/tests/test_ssh_deadline drives each SSH operation against a
//! control socket that accepts and never answers, and a fresh-master start
//! shares its total deadline with an `ssh` that never authenticates.
use crate::runner::{self, Case, case};
use crate::support::bounded_process;
use crate::support::pty;
use crate::support::tmp::TempDir;
use crate::support::tui::install_fixture;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The fixture that stands in for `ssh` in the fresh-master case.
pub const UNRESPONSIVE_SSH: &str = "ssh-deadline-unresponsive";

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = Vec::new();
    for (operation, budget) in [("alive", 5.0), ("exit", 5.0), ("start", 1.0), ("forward", 5.0), ("cancel", 5.0), ("exec", 0.2)] {
        cases.push(case(format!("unresponsive_master/{operation}"), move || {
            unresponsive_master(operation, Duration::from_secs_f64(budget))
        }));
    }
    cases.push(case("fresh_master_start_shares_the_total_deadline", fresh_master_start_shares_the_total_deadline));
    runner::run("ssh-deadline", "real SSH check/exit/start/forward/cancel/exec obey operation deadlines", cases, filters)
}

/// `$SSH_DEADLINE_TEST`, or build/tests/test_ssh_deadline.
fn binary() -> PathBuf {
    let path =
        std::env::var_os("SSH_DEADLINE_TEST").map_or_else(|| PathBuf::from("build/tests/test_ssh_deadline"), PathBuf::from);
    std::fs::canonicalize(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn unresponsive_master(operation: &str, budget: Duration) {
    let binary = binary();
    let directory = TempDir::new("hamn-ssh-");
    let server = SilentMaster::start(&directory.path().join("ssh.sock"));
    let started = Instant::now();
    let result = bounded_process::output(
        Command::new(&binary).arg(directory.path()).arg(operation).env("PATH", "/usr/bin:/bin"),
        budget + Duration::from_secs(4),
    );
    let elapsed = started.elapsed();
    assert!(server.accepted(), "{operation}: {result:?}");
    assert_eq!(result.status.code(), Some(0), "{operation}: {result:?}");
    assert!(budget <= elapsed && elapsed < budget + Duration::from_secs(4), "{operation}: {elapsed:?}");
    if operation == "forward" {
        let text = String::from_utf8_lossy(&result.stdout);
        assert_eq!(text.matches("completion observed failure").count(), 1, "{result:?}");
    }
    server.finish();
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

/// An SSH control socket that accepts one connection and holds it, without
/// a byte, until released (at most 12 seconds). No connection within 10
/// seconds is an error.
struct SilentMaster {
    accepted: Arc<AtomicBool>,
    errors: Arc<Mutex<Vec<String>>>,
    release: mpsc::Sender<()>,
    done: mpsc::Receiver<()>,
}

impl SilentMaster {
    fn start(path: &Path) -> Self {
        let listener = UnixListener::bind(path).unwrap_or_else(|error| panic!("bind {}: {error}", path.display()));
        // The Python fixture listened with a backlog of one; std uses 128.
        // SAFETY: listen on a descriptor this listener owns only changes
        // its backlog.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0, "listen: {}", std::io::Error::last_os_error());
        let accepted = Arc::new(AtomicBool::new(false));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = mpsc::channel::<()>();
        let (finished, done) = mpsc::channel();
        let (flag, failures) = (Arc::clone(&accepted), Arc::clone(&errors));
        std::thread::spawn(move || {
            let result = if pty::readable(&[listener.as_raw_fd()], Duration::from_secs(10)).is_empty() {
                Err("no connection within 10 seconds".to_owned())
            } else {
                listener.accept().map_err(|error| format!("accept: {error}")).and_then(|(connection, _)| {
                    flag.store(true, Ordering::SeqCst);
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

    fn accepted(&self) -> bool {
        self.accepted.load(Ordering::SeqCst)
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
