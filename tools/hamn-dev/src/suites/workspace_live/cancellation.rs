//! Deterministic guest lock barriers for real navigation, cancellation and
//! forced worker exit: a test-held deployment lock keeps a start's guest
//! recovery waiting while the frontend is navigated, quit, or its worker
//! killed.
use super::processes::System;
use super::terminal::{Driver, Terminal};
use super::transport::owned_worker;
use super::{
    Live, Must, PROFILE, check_interrupt, communicate, finally, path_str, read_record, readable, signal_child,
};
use crate::release::syntax::shell_join;
use crate::support::pty;
use serde_json::Value;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Waits up to `timeout` until the operation record at `path` satisfies
/// `predicate` and returns it ({} while no record exists). The record's
/// directory is watched with kqueue, since the product replaces the record
/// by rename; `terminal`, when given, keeps being read and rendered.
pub(crate) fn wait_record(
    path: &Path,
    predicate: impl Fn(&Value) -> bool,
    timeout: Duration,
    mut terminal: Option<&mut Terminal>,
) -> Value {
    let directory = path.parent().expect("record directory");
    let directory = File::open(directory).unwrap_or_else(|error| panic!("{}: {error}", directory.display()));
    // SAFETY: kqueue returns a new descriptor or -1.
    let queue = unsafe { libc::kqueue() };
    assert!(queue >= 0, "kqueue: {}", std::io::Error::last_os_error());
    // SAFETY: queue is a new descriptor owned here.
    let queue = unsafe { OwnedFd::from_raw_fd(queue) };
    let change = libc::kevent {
        ident: directory.as_raw_fd() as libc::uintptr_t,
        filter: libc::EVFILT_VNODE,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: libc::NOTE_WRITE,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one initialized change and no event buffer.
    let registered = unsafe { libc::kevent(queue.as_raw_fd(), &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    assert_eq!(registered, 0, "kevent: {}", std::io::Error::last_os_error());
    let deadline = Instant::now() + timeout;
    let mut terminal_open = true;
    loop {
        let value = read_record(path);
        if predicate(&value) {
            return value;
        }
        check_interrupt();
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "{value}");
        let mut fds = vec![queue.as_raw_fd()];
        if let Some(terminal) = terminal.as_deref()
            && terminal_open
        {
            fds.push(terminal.master());
        }
        let ready = readable(&fds, left);
        if ready.contains(&queue.as_raw_fd()) {
            // SAFETY: kevent is plain data; the call writes at most one.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            // SAFETY: no changes, one writable event, a zero timeout.
            unsafe { libc::kevent(queue.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &now) };
        }
        if let Some(terminal) = terminal.as_deref_mut()
            && ready.contains(&terminal.master())
        {
            // The TUI may exit (and its PTY close) once the operation ends.
            terminal_open = terminal.pump();
        }
    }
}

/// Holds the guest deployment lock from a test SSH session until released.
struct Gate<'a> {
    live: &'a Live,
    child: Child,
    released: bool,
}

const GATE_DIRECTORY: &str = "/run/hamn-workspace-gate";

impl<'a> Gate<'a> {
    fn new(live: &'a Live) -> Self {
        let status = live.call(&["vm", "status"], &[]);
        let ip = status["ip"].as_str().expect("VM status has no IP address").to_owned();
        live.ssh(&format!("mkdir -m 700 {GATE_DIRECTORY}; mkfifo {GATE_DIRECTORY}/release"));
        let holder = format!("echo LOCK_READY; read token < {GATE_DIRECTORY}/release; test \"$token\" = release");
        let remote = shell_join(&["sudo", "flock", "/run/hamn-deployment.lock", "bash", "-c", &holder]);
        let key = live.profile().join("id_ed25519");
        let child = Command::new("/usr/bin/ssh")
            .args(["-F", "none", "-i", path_str(&key)])
            .args(["-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "StrictHostKeyChecking=no"])
            .args(["-o", "UserKnownHostsFile=/dev/null", &format!("hamn@{ip}"), &remote])
            .env_clear()
            .envs(&live.runtime.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the lock holder");
        // Constructed first, so a failed wait still ends the holder.
        let mut gate = Self { live, child, released: false };
        let stdout = gate.child.stdout.as_mut().expect("piped output");
        let line = read_line(stdout, Duration::from_secs(15));
        assert_eq!(line, "LOCK_READY\n", "the lock holder did not take the deployment lock");
        gate
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.live.ssh(&format!("printf \"release\\n\" > {GATE_DIRECTORY}/release"));
        let status = crate::support::exec::wait_timeout(&mut self.child, Duration::from_secs(15));
        assert!(status.is_some_and(|status| status.success()), "the lock holder failed: {status:?}");
        self.live.ssh(&format!("rm {GATE_DIRECTORY}/release; rmdir {GATE_DIRECTORY}"));
        self.released = true;
    }
}

impl Drop for Gate<'_> {
    fn drop(&mut self) {
        // Our own unreaped SSH child; its guest lock ends with the session.
        if !self.released && super::running(&mut self.child) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// One line from `stdout` within `timeout`.
pub(crate) fn read_line(stdout: &mut ChildStdout, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();
    while !line.ends_with(b"\n") {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no line within {timeout:?}: {:?}", String::from_utf8_lossy(&line));
        if readable(&[stdout.as_raw_fd()], left).is_empty() {
            continue;
        }
        let mut byte = [0u8];
        match stdout.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => line.push(byte[0]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => panic!("read: {error}"),
        }
    }
    String::from_utf8_lossy(&line).into_owned()
}

/// `hamn --headless vm start --profile PROFILE FLAGS... --yes` with piped
/// output.
pub(crate) fn headless_start(live: &Live, profile: &str, flags: &[&str]) -> Child {
    Command::new(&live.runtime.binary)
        .args(["--headless", "vm", "start", "--profile", profile])
        .args(flags)
        .arg("--yes")
        .env_clear()
        .envs(&live.runtime.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start headless vm start")
}

/// Navigation keeps an active start running; a confirmed quit waits for
/// remote cleanup; the existing VM survives. Then a forced worker death
/// leaves `outcomeUnknown`, and a retry repairs Docker without replacing the
/// VM.
pub(crate) fn cancellation(live: &Live) {
    let path = live.profile().join("operation.json");
    let vm_pid = live.vm_pid();
    let mut results: Vec<Value> = Vec::new();
    let mut session = (Gate::new(live), Terminal::new(&live.runtime.binary, &live.runtime.environment, &live.root));
    finally(
        &mut session,
        |(gate, terminal)| {
            if !live.runtime.home.join(".hamn/tui.json").exists() {
                terminal.until("Choose your default workspace");
                terminal.send(b"1\r", None);
            }
            terminal.until("hamn-workspace-sentinel");
            terminal.send(b":vm start --profile verify\r", Some("Confirm vm start"));
            terminal.send(b"y!", Some("recovering-deployment"));
            let active = wait_record(
                &path,
                |value| value["phase"] == "recovering-deployment" && value["status"] == "running",
                Duration::from_secs(30),
                None,
            );
            terminal.send(b"\x1b\t", Some("Kubernetes"));
            let current = read_record(&path);
            assert_eq!(current["operationId"], active["operationId"], "navigation replaced the operation");
            assert_eq!(current["status"], "running", "navigation ended the operation");
            terminal.send(b"q", Some("Cancel the active operation and exit?"));
            terminal.send(b"y", None);
            let fencing = |value: &Value| value["phase"] == "fencing-after-cancel";
            wait_record(&path, fencing, Duration::from_secs(30), Some(&mut *terminal));
            assert!(terminal.running(), "quit did not wait for remote cleanup");
            gate.release();
            let completed =
                wait_record(&path, |value| value["status"] != "running", Duration::from_secs(60), Some(&mut *terminal));
            assert_eq!(completed["status"], "cancelled", "{completed}");
            results.push(completed);
        },
        |(gate, terminal)| {
            gate.release();
            terminal.close();
        },
    );
    drop(session);
    assert_eq!(live.vm_pid(), vm_pid, "cancellation replaced the existing VM");
    assert_eq!(live.call(&["vm", "status"], &[])["dockerStatus"], "ready");
    println!("PASS: navigation preserves start; confirmed quit waits for remote cleanup; existing VM survives");

    let mut session = (Gate::new(live), headless_start(live, PROFILE, &[]));
    finally(
        &mut session,
        |(gate, child)| {
            let previous = results.last().expect("the cancelled operation")["operationId"].clone();
            let active = wait_record(
                &path,
                |value| value["operationId"] != previous && value["phase"] == "recovering-deployment",
                Duration::from_secs(30),
                None,
            );
            // Only this start's own worker: the frontend's child running the
            // candidate, with the birth and executable its record names.
            let worker = owned_worker(&System, child.id(), &active, &live.runtime.binary).must();
            pty::kill(worker.pid as u32, libc::SIGKILL);
            let (status, stdout, stderr) = communicate(child, Duration::from_secs(15));
            assert!(!status.success(), "the frontend succeeded without its worker: {stdout} {stderr}");
            let result: Value = serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{error}: {stdout}"));
            assert_eq!(result["error"]["code"], "outcomeUnknown", "{result}");
            let status = live.call(&["vm", "status"], &[]);
            assert_eq!(status["dockerStatus"], "recoveryRequired", "{status}");
            gate.release();
            let result = live.call(&["vm", "start"], &["--yes"]);
            assert_eq!(result["dockerStatus"], "ready", "{result}");
            assert_eq!(live.vm_pid(), vm_pid, "the retry replaced the existing VM");
            results.push(result);
        },
        |(gate, child)| {
            gate.release();
            signal_child(child, libc::SIGTERM);
            if crate::support::exec::wait_timeout(child, Duration::from_secs(15)).is_none() {
                // Our own unreaped child; reaped before the failure is reported.
                let _ = child.kill();
                let _ = child.wait();
                panic!("the headless start survived SIGTERM for 15 s");
            }
        },
    );
    live.write_json("cancellation-results.json", &Value::from(results));
    println!("PASS: forced worker death retains outcomeUnknown; retry repairs without terminating the existing VM");
}

/// A cancelled start stops only the VM it created itself.
pub(crate) fn owned_start_cancellation(live: &Live) {
    let profile = "cancel-owned";
    let directory = live.runtime.profile_dir(profile);
    match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("{}: {error}", directory.display()),
    }
    let path = directory.join("operation.json");
    let previous = read_record(&path)["operationId"].clone();
    let mut child = headless_start(live, profile, &["--cpu", "2", "--memory", "2", "--disk", "60"]);
    finally(
        &mut child,
        |child| {
            wait_record(
                &path,
                |value| value["operationId"] != previous && value["startedVm"] == true,
                Duration::from_secs(180),
                None,
            );
            signal_child(child, libc::SIGINT);
            communicate(child, Duration::from_secs(180));
            let status = live.runtime.call(&["vm", "status"], profile, &[]).must();
            assert_eq!(status["state"], "stopped", "{status}");
            assert_eq!(status["lastOperation"]["status"], "cancelled", "{status}");
            live.write_json("owned-cancel-result.json", &status);
            println!("PASS: cancelled start stops only its newly created VM");
        },
        |child| {
            if super::running(child) {
                signal_child(child, libc::SIGINT);
                communicate(child, Duration::from_secs(180));
            }
            live.runtime.stop(&[profile.to_owned()]).must();
        },
    );
}
