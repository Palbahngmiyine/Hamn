//! A structured query owns one process group, including inherited CLI helpers.
//! Observe leader exit without reaping it: its PID must remain reserved until
//! the group is terminated. Completion, read failure and cancellation all clean
//! the group and reap its leader. Deliberately detached sessions are not owned.
//! A cleanup failure that no caller receives (on drop) goes to stderr, or is
//! held for the TUI while it draws on the terminal ([`hold_reports`]).
use std::{
    io,
    process::{ExitStatus, Stdio},
    sync::{Mutex, MutexGuard, PoisonError},
};
use tokio::{
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    signal::unix::{Signal, SignalKind},
};

pub struct QueryProcess {
    child: Child,
    pid: libc::pid_t,
    exited: Signal,
    armed: bool,
}

impl QueryProcess {
    pub fn spawn(command: Command) -> io::Result<(Self, ChildStdout, ChildStderr)> {
        Self::spawn_with_input(command, false)
    }

    /// A caller owns the input pipe; dropping the process still terminates and
    /// reaps its entire original group before the PID can be reused.
    pub fn spawn_piped(
        command: Command,
    ) -> io::Result<(Self, ChildStdin, ChildStdout, ChildStderr)> {
        let (mut process, stdout, stderr) = Self::spawn_with_input(command, true)?;
        let stdin = process.child.stdin.take().expect("query stdin is piped");
        Ok((process, stdin, stdout, stderr))
    }

    fn spawn_with_input(
        mut command: Command,
        piped: bool,
    ) -> io::Result<(Self, ChildStdout, ChildStderr)> {
        // Subscribe before spawning; the first waitid also catches a child that
        // exits before the signal stream is polled. Other children's signals
        // only cause another observation of this exact, unreaped child.
        let exited = tokio::signal::unix::signal(SignalKind::child())?;
        let mut child = command
            .stdin(if piped { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(false)
            .spawn()?;
        let pid = child.id().expect("new child has a PID") as libc::pid_t;
        let stdout = child.stdout.take().expect("query stdout is piped");
        let stderr = child.stderr.take().expect("query stderr is piped");
        Ok((
            Self {
                child,
                pid,
                exited,
                armed: true,
            },
            stdout,
            stderr,
        ))
    }

    fn terminate_group(&self) -> io::Result<()> {
        // The leader may have moved to another group. It remains our exact
        // unreaped child; never signal its new group, which may contain the UI.
        if unsafe { libc::kill(self.pid, libc::SIGKILL) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        // The leader is still our unreaped child, so this PGID cannot refer to
        // a new unrelated group.
        if unsafe { libc::kill(-self.pid, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        group_gone(
            io::Error::last_os_error(),
            || self.await_exit(),
            || self.only_leader_listed(),
        )
    }

    /// Whether the group lists the leader and no other member. A two-slot
    /// enumeration is enough to prove that there is no other.
    #[cfg(target_os = "macos")]
    fn only_leader_listed(&self) -> bool {
        let mut members = [0 as libc::pid_t; 2];
        let count = unsafe {
            libc::proc_listpgrppids(
                self.pid,
                members.as_mut_ptr().cast(),
                std::mem::size_of_val(&members) as libc::c_int,
            )
        };
        count == 1 && members[0] == self.pid
    }

    #[cfg(not(target_os = "macos"))]
    fn only_leader_listed(&self) -> bool {
        false
    }

    fn has_exited(&self) -> io::Result<bool> {
        self.observe_exit(libc::WNOHANG)
    }

    /// Waits until waitid sees the leader exited, without reaping it. Only
    /// for a leader that has had SIGKILL: its exit bounds the wait.
    fn await_exit(&self) -> io::Result<()> {
        if self.observe_exit(0)? {
            Ok(())
        } else {
            Err(io::Error::other(
                "waitid returned before the query leader exited",
            ))
        }
    }

    fn observe_exit(&self, options: libc::c_int) -> io::Result<bool> {
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | options,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(error);
            }
            return Ok(unsafe { info.si_pid() } == self.pid);
        }
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        while !self.has_exited()? {
            self.exited
                .recv()
                .await
                .ok_or_else(|| io::Error::other("query exit signal stream closed"))?;
        }
        // Descendants may still hold output pipes after the CLI exits. Stop
        // them before reaping, then let the reader drain the finite queued tail.
        self.terminate_group()?;
        let status = self.child.wait().await?;
        self.armed = false;
        Ok(status)
    }
}

/// Whether a group whose `kill(-pgid, SIGKILL)` failed with `error` is gone
/// all the same, after the leader itself has had SIGKILL. A vanished group
/// (ESRCH) is. On Darwin, killpg returns EPERM, having signalled no one,
/// when the only member is a leader that exits on its own: while it exits,
/// before waitid can see the exit, as well as once it is a zombie. The
/// leader is then waited for (`await_exit`), and a group that lists it
/// alone holds nothing else to stop. Any other member, such as one that
/// cannot be signalled, keeps the EPERM: a real permission failure is never
/// hidden. Every other error stays as it is.
fn group_gone(
    error: io::Error,
    await_exit: impl FnOnce() -> io::Result<()>,
    only_leader_listed: impl FnOnce() -> bool,
) -> io::Result<()> {
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(()),
        Some(libc::EPERM) if cfg!(target_os = "macos") => {
            await_exit()?;
            if only_leader_listed() {
                Ok(())
            } else {
                Err(error)
            }
        }
        _ => Err(error),
    }
}

/// Failures of a cleanup that has no caller to return them to (`Drop`):
/// `None` sends each to stderr at once; `Some` holds them for the TUI. While
/// the TUI draws on the terminal, a line on stderr would land in the middle
/// of its screen.
static HELD_REPORTS: Mutex<Option<Vec<String>>> = Mutex::new(None);

fn held_reports() -> MutexGuard<'static, Option<Vec<String>>> {
    HELD_REPORTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// From now on, holds cleanup failures for [`take_reports`] instead of
/// writing them to stderr. The TUI calls it once it owns the terminal.
pub fn hold_reports() {
    held_reports().get_or_insert_with(Vec::new);
}

/// The cleanup failures held since the last call, oldest first.
pub fn take_reports() -> Vec<String> {
    held_reports()
        .as_mut()
        .map(std::mem::take)
        .unwrap_or_default()
}

/// Stops holding cleanup failures and returns those still held, for the
/// TUI to print once it has given the terminal back.
pub fn release_reports() -> Vec<String> {
    held_reports().take().unwrap_or_default()
}

fn report(message: String) {
    match held_reports().as_mut() {
        Some(held) => held.push(message),
        None => eprintln!("hamn: {message}"),
    }
}

impl Drop for QueryProcess {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Err(error) = self.terminate_group() {
            report(format!("cannot terminate query process group: {error}"));
        }
        // Cancellation also reaps before runtime shutdown. No other caller
        // waits on this leader, and SIGKILL has already settled its lifetime.
        loop {
            if unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), 0) } >= 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                report(format!("cannot reap query process: {error}"));
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// A query cancelled the moment it finishes: its leader has written its
    /// last output and exits on its own while the group is cleaned up. The
    /// cleanup is complete; it must not fail.
    #[tokio::test]
    async fn a_leader_exiting_on_its_own_is_cleaned_up_without_a_failure() {
        for round in 0..200 {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "printf x"]);
            let (process, mut stdout, _stderr) = QueryProcess::spawn(command).unwrap();
            let mut byte = [0];
            stdout.read_exact(&mut byte).await.unwrap();
            if let Err(error) = process.terminate_group() {
                panic!("round {round}: {error}");
            }
        }
    }

    /// The decision after a failed `kill(-pgid)`, with the calls answered in
    /// a chosen order: what is called, in which order, and the result.
    #[test]
    fn a_failed_group_kill_waits_for_the_leader_before_it_counts_members() {
        let errno = |code| io::Error::from_raw_os_error(code);
        let run = |error: io::Error, waited: io::Result<()>, only_leader: bool| {
            let calls = std::cell::RefCell::new(Vec::new());
            let result = group_gone(
                error,
                || {
                    calls.borrow_mut().push("await exit");
                    waited
                },
                || {
                    calls.borrow_mut().push("list members");
                    only_leader
                },
            );
            (
                result.map_err(|error| error.raw_os_error()),
                calls.into_inner(),
            )
        };
        // A vanished group needs nothing more.
        assert_eq!(run(errno(libc::ESRCH), Ok(()), false), (Ok(()), vec![]));
        // An exiting or exited leader alone: the cleanup is complete, and the
        // members are counted only once the leader is seen exited.
        assert_eq!(
            run(errno(libc::EPERM), Ok(()), true),
            (Ok(()), vec!["await exit", "list members"])
        );
        // Another member keeps the permission failure.
        assert_eq!(
            run(errno(libc::EPERM), Ok(()), false),
            (Err(Some(libc::EPERM)), vec!["await exit", "list members"])
        );
        // A wait that fails is that failure; nothing is counted.
        assert_eq!(
            run(errno(libc::EPERM), Err(errno(libc::ECHILD)), true),
            (Err(Some(libc::ECHILD)), vec!["await exit"])
        );
        // Any other error is kept without a wait.
        assert_eq!(
            run(errno(libc::EINVAL), Ok(()), true),
            (Err(Some(libc::EINVAL)), vec![])
        );
    }

    /// While the TUI holds them, cleanup failures wait for it in order; once
    /// released, the rest are handed back and nothing more is held.
    #[test]
    fn held_cleanup_failures_wait_for_the_screen() {
        hold_reports();
        report("first".into());
        report("second".into());
        assert_eq!(take_reports(), ["first", "second"]);
        assert!(take_reports().is_empty());
        report("third".into());
        assert_eq!(release_reports(), ["third"]);
        // Released: a failure goes to stderr and is not kept.
        report("written to stderr".into());
        assert!(take_reports().is_empty() && release_reports().is_empty());
    }
}
