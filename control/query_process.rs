//! A structured query owns one process group, including inherited CLI helpers.
//! Observe leader exit without reaping it: its PID must remain reserved until
//! the group is terminated. Completion, read failure and cancellation all clean
//! the group and reap its leader. Deliberately detached sessions are not owned.
use std::{io, process::{ExitStatus, Stdio}};
use tokio::{process::{Child, ChildStderr, ChildStdout, Command}, signal::unix::{Signal, SignalKind}};

pub struct QueryProcess {
    child: Child,
    pid: libc::pid_t,
    exited: Signal,
    armed: bool,
}

impl QueryProcess {
    pub fn spawn(mut command: Command) -> io::Result<(Self, ChildStdout, ChildStderr)> {
        // Subscribe before spawning; the first waitid also catches a child that
        // exits before the signal stream is polled. Other children's signals
        // only cause another observation of this exact, unreaped child.
        let exited = tokio::signal::unix::signal(SignalKind::child())?;
        let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped())
            .stderr(Stdio::piped()).process_group(0).kill_on_drop(false).spawn()?;
        let pid = child.id().expect("new child has a PID") as libc::pid_t;
        let stdout = child.stdout.take().expect("query stdout is piped");
        let stderr = child.stderr.take().expect("query stderr is piped");
        Ok((Self { child, pid, exited, armed: true }, stdout, stderr))
    }

    fn terminate_group(&self) -> io::Result<()> {
        // The leader may have moved to another group. It remains our exact
        // unreaped child; never signal its new group, which may contain the UI.
        if unsafe { libc::kill(self.pid, libc::SIGKILL) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) { return Err(error); }
        }
        // The leader is still our unreaped child, so this PGID cannot refer to
        // a new unrelated group. A vanished group already satisfies cleanup.
        if unsafe { libc::kill(-self.pid, libc::SIGKILL) } == 0 { return Ok(()); }
        let error = io::Error::last_os_error();
        // Darwin's killpg1 skips zombies and returns EPERM even when only the
        // exited leader remains. Do not hide a real permission failure: a
        // two-slot enumeration must prove there is no other group member.
        #[cfg(target_os = "macos")]
        if error.raw_os_error() == Some(libc::EPERM) && self.has_exited()? {
            let mut members = [0 as libc::pid_t; 2];
            let count = unsafe { libc::proc_listpgrppids(self.pid, members.as_mut_ptr().cast(),
                std::mem::size_of_val(&members) as libc::c_int) };
            if count == 1 && members[0] == self.pid { return Ok(()); }
        }
        if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) }
    }

    fn has_exited(&self) -> io::Result<bool> {
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::waitid(libc::P_PID, self.pid as libc::id_t,
                &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) { continue; }
                return Err(error);
            }
            return Ok(unsafe { info.si_pid() } == self.pid);
        }
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        while !self.has_exited()? {
            self.exited.recv().await.ok_or_else(|| io::Error::other("query exit signal stream closed"))?;
        }
        // Descendants may still hold output pipes after the CLI exits. Stop
        // them before reaping, then let the reader drain the finite queued tail.
        self.terminate_group()?;
        let status = self.child.wait().await?;
        self.armed = false;
        Ok(status)
    }
}

impl Drop for QueryProcess {
    fn drop(&mut self) {
        if !self.armed { return; }
        if let Err(error) = self.terminate_group() {
            eprintln!("hamn: cannot terminate query process group: {error}");
        }
        // Cancellation also reaps before runtime shutdown. No other caller
        // waits on this leader, and SIGKILL has already settled its lifetime.
        loop {
            if unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), 0) } >= 0 { break; }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                eprintln!("hamn: cannot reap query process: {error}");
                break;
            }
        }
    }
}
