use crate::{
    native::Invocation,
    terminal_io::{self, Replies},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    widgets::Paragraph,
};
use std::{
    collections::VecDeque,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    process::Stdio,
    sync::Arc,
};
use tokio::{io::unix::AsyncFd, sync::mpsc};

const INPUT_LIMIT: usize = 4 * 1024 * 1024;
pub enum Event {
    Output(Vec<u8>),
    Exited(i32),
    Ended,
    InputProgress,
}
pub struct Session {
    pub id: u64,
    pub invocation: Invocation,
    pub parser: vt100::Parser<Replies>,
    pub exit: Option<i32>,
    child: tokio::process::Child,
    pid: libc::pid_t,
    exited: tokio::signal::unix::Signal,
    armed: bool,
    master: Arc<AsyncFd<OwnedFd>>,
    output: mpsc::Receiver<io::Result<Vec<u8>>>,
    reader: tokio::task::JoinHandle<()>,
    ended: bool,
    input: VecDeque<u8>,
    input_closed: bool,
    input_error: Option<String>,
}
impl Session {
    pub fn summary(&self) -> String {
        session_summary(&self.invocation)
    }
    pub fn start(invocation: Invocation, width: u16, height: u16) -> io::Result<Self> {
        let mut command = invocation.command(false);
        let _input = match invocation.body.as_deref() {
            Some(body) => {
                let (file, path) = crate::command_input::attach(&mut command, body)?;
                command.args(["--filename", &path]);
                Some(file)
            }
            None => None,
        };
        Self::spawn(command, invocation, width, height)
    }
    fn spawn(
        mut command: tokio::process::Command,
        invocation: Invocation,
        width: u16,
        height: u16,
    ) -> io::Result<Self> {
        let (rows, cols) = (height.saturating_sub(3).max(1), width.max(1));
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let (mut master, mut slave) = (-1, -1);
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        command.env("TERM", "xterm-256color").kill_on_drop(false);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let exited = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
        let master = Arc::new(AsyncFd::new(master)?);
        let child = command.spawn()?;
        let pid = child.id().expect("new PTY child has a PID") as libc::pid_t;
        let source = master.clone();
        let (sender, output) = mpsc::channel(64);
        let reader = tokio::spawn(async move {
            loop {
                let result = async {
                    let mut ready = source.readable().await?;
                    let mut bytes = vec![0; 8192];
                    let read = ready.try_io(|fd| {
                        let count = unsafe {
                            libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len())
                        };
                        if count < 0 {
                            Err(io::Error::last_os_error())
                        } else {
                            Ok(count as usize)
                        }
                    });
                    match read {
                        Ok(Ok(n)) => {
                            bytes.truncate(n);
                            Ok(Some(bytes))
                        }
                        Ok(Err(e)) => Err(e),
                        Err(_) => Ok(None),
                    }
                }
                .await;
                match result {
                    Ok(Some(bytes)) if bytes.is_empty() => return,
                    Ok(Some(bytes)) => {
                        if sender.send(Ok(bytes)).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => return,
                    Err(e) => {
                        let _ = sender.send(Err(e)).await;
                        return;
                    }
                }
            }
        });
        static SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Ok(Self {
            id: SESSION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            invocation,
            parser: vt100::Parser::new_with_callbacks(rows, cols, 10000, Replies::default()),
            exit: None,
            child,
            pid,
            exited,
            armed: true,
            master,
            output,
            reader,
            ended: false,
            input: VecDeque::new(),
            input_closed: false,
            input_error: None,
        })
    }
    pub async fn next(&mut self) -> io::Result<Event> {
        let pending_input = !self.input.is_empty() && !self.input_closed;
        tokio::select! {
            result = self.output.recv(), if !self.ended => match result {
                Some(bytes) => Ok(Event::Output(bytes?)),
                None => { self.ended = true; Ok(Event::Ended) },
            },
            result = wait_owned(&mut self.child, self.pid, &mut self.exited), if self.exit.is_none() => {
                use std::os::unix::process::ExitStatusExt;
                let status = result?;
                let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                self.armed = false;
                self.exit = Some(code);
                self.close_input("CLI exited");
                Ok(Event::Exited(code))
            },
            result = flush_input(&self.master, &mut self.input), if pending_input => {
                if let Err(error) = result { self.close_input(&format!("PTY write failed: {error}")); }
                Ok(Event::InputProgress)
            },
            else => std::future::pending().await,
        }
    }
    fn close_input(&mut self, reason: &str) {
        if !self.input.is_empty() {
            self.input_error = Some(format!(
                "{reason}: {} queued input bytes were not delivered",
                self.input.len()
            ));
        }
        self.input.clear();
        self.input_closed = true;
    }
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.input_closed || bytes.len() > INPUT_LIMIT.saturating_sub(self.input.len()) {
            let reason = if self.input_closed {
                "PTY input is closed"
            } else {
                "4 MiB input queue is full; wait and retry"
            };
            self.input_error = Some(format!("{reason}: {} new bytes were not sent", bytes.len()));
            return Err(io::Error::other(self.input_error.as_ref().unwrap().clone()));
        }
        self.input.extend(bytes);
        Ok(())
    }
    pub fn scroll_key(&mut self, key: KeyEvent) -> bool {
        if !matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
            || !(key.modifiers == KeyModifiers::SHIFT
                || (self.exit.is_some() && key.modifiers.is_empty()))
        {
            return false;
        }
        let screen = self.parser.screen_mut();
        let page = usize::from(screen.size().0.saturating_sub(1).max(1));
        let offset = if key.code == KeyCode::PageUp {
            screen.scrollback().saturating_add(page)
        } else {
            screen.scrollback().saturating_sub(page)
        };
        screen.set_scrollback(offset);
        true
    }
    pub fn input(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.parser.screen_mut().set_scrollback(0);
        self.write(bytes)
    }
    pub fn interrupt_input(&mut self) -> io::Result<()> {
        let pending = self.input.len();
        self.input.clear();
        let flushed = unsafe { libc::tcflush(self.master.as_raw_fd(), libc::TCIFLUSH) };
        self.input_error = Some(format!(
            "Interrupted: discarded {pending} queued bytes and pending terminal input"
        ));
        let signal = self.signal(libc::SIGINT);
        if let Err(error) = &signal {
            self.input_error = Some(format!(
                "Interrupt failed: {error}; {pending} queued bytes discarded"
            ));
        } else if flushed < 0 {
            self.input_error = Some(format!(
                "Interrupted; {pending} queued bytes discarded; terminal flush failed"
            ));
        }
        signal
    }
    pub fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        let (rows, cols) = (height.saturating_sub(3).max(1), width.max(1));
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(io::Error::last_os_error());
        }
        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }
    pub fn signal(&mut self, signal: i32) -> io::Result<()> {
        if self.armed && unsafe { libc::kill(-self.pid, signal) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
    pub fn draw(&self, frame: &mut ratatui::Frame) {
        let areas = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
        frame.render_widget(
            Paragraph::new(crate::tui_state::clean(&format!(
                "Hamn | {} terminal\n{}",
                self.invocation.program(),
                self.invocation.target
            ))),
            areas[0],
        );
        frame.render_widget(terminal_io::Screen(self.parser.screen()), areas[1]);
        let status = self.exit.map(|code| format!("Exit code {code} | PgUp/PgDn scroll | Enter / Esc returns to the resource list"))
            .unwrap_or_else(|| "Input to CLI | Ctrl+Alt+B browser | Ctrl+Alt+S sessions | Ctrl+Alt+C interrupt | Shift+PgUp/PgDn scroll".into());
        let status = self
            .input_error
            .as_ref()
            .map_or(status.clone(), |error| match self.exit {
                Some(code) => format!("Exit code {code} | {error} | Enter/Esc returns"),
                None => format!("{error} | Ctrl+Alt+C interrupts"),
            });
        frame.render_widget(Paragraph::new(status), areas[2]);
        if !self.parser.screen().hide_cursor()
            && self.exit.is_none()
            && self.parser.screen().scrollback() == 0
        {
            let (row, col) = self.parser.screen().cursor_position();
            frame.set_cursor_position((areas[1].x + col, areas[1].y + row));
        }
    }
}
/// A transient, bounded label for known log/forward operands, never raw argv.
/// Skip consumed credential/connection values; unknown flag arity falls back to
/// the command name rather than risking an option value becoming a resource.
fn session_summary(invocation: &Invocation) -> String {
    use crate::preferences::Workspace;
    fn short(value: &str, limit: usize) -> String {
        let mut result: String = value.chars().take(limit).collect();
        if value.chars().count() > limit {
            result.push('…');
        }
        result
    }
    fn identifier(value: &str) -> bool {
        value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._/".contains(&b))
    }
    let workspace = invocation.workspace;
    let args = crate::native_flags::inspect(&invocation.args, workspace);
    let Some(index) = crate::native::command_index(&args, workspace) else {
        return invocation.program().into();
    };
    let mut command = args[index].as_str();
    let mut index = index + 1;
    if workspace == Workspace::Containers
        && command == "container"
        && args.get(index).is_some_and(|arg| arg == "logs")
    {
        command = "logs";
        index += 1;
    }
    let base = format!(
        "{} {}",
        invocation.program(),
        short(&crate::tui_state::clean(command), 32)
    );
    let forward = workspace == Workspace::Kubernetes && command == "port-forward";
    if command != "logs" && !forward {
        return base;
    }
    let takes_value = |flag: &str| {
        if workspace == Workspace::Kubernetes {
            crate::native_flags::takes_value_in(flag, workspace, command)
        } else {
            ["--since", "--until", "--tail", "-n"].contains(&flag)
        }
    };
    let mut operands = Vec::new();
    let mut container = None;
    let mut positional = false;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if arg == "--" && !positional {
            positional = true;
            continue;
        }
        if positional || !arg.starts_with('-') {
            operands.push(arg);
            continue;
        }
        let (flag, inline) = if let Some((flag, value)) = arg.split_once('=') {
            (flag, Some(value))
        } else if arg.len() > 2
            && !arg.starts_with("--")
            && arg.is_char_boundary(2)
            && takes_value(&arg[..2])
        {
            (&arg[..2], Some(&arg[2..]))
        } else {
            (arg, None)
        };
        if takes_value(flag) {
            let value = match inline {
                Some(value) => value,
                None => {
                    let Some(value) = args.get(index) else {
                        return base;
                    };
                    index += 1;
                    value.as_str()
                }
            };
            if workspace == Workspace::Kubernetes
                && ["-c", "--container"].contains(&flag)
                && identifier(value)
            {
                container = Some(value);
            }
        } else {
            let known_log_flag = command == "logs"
                && if workspace == Workspace::Kubernetes {
                    [
                        "--follow",
                        "-f",
                        "--previous",
                        "-p",
                        "--timestamps",
                        "--all-containers",
                        "--all-pods",
                        "--ignore-errors",
                        "--prefix",
                        "--insecure-skip-tls-verify-backend",
                    ]
                    .contains(&flag)
                } else {
                    ["--follow", "-f", "--timestamps", "-t", "--details"].contains(&flag)
                        || (flag.starts_with('-')
                            && !flag.starts_with("--")
                            && flag[1..].chars().all(|c| c == 'f' || c == 't'))
                };
            if !known_log_flag
                && !["--help", "-h"].contains(&flag)
                && !(workspace == Workspace::Kubernetes
                    && crate::native::KUBE_CONNECTION_FLAGS.contains(&flag))
            {
                return base;
            }
        }
    }
    let Some(resource) = operands.first().filter(|value| identifier(value)) else {
        return base;
    };
    if !forward && operands.len() != 1 {
        return base;
    }
    let mut label = format!("{base} {}", short(resource, 48));
    if forward {
        if operands.len() < 2
            || operands[1..].iter().any(|value| {
                value.is_empty()
                    || !value.bytes().any(|b| b.is_ascii_alphanumeric())
                    || value.bytes().filter(|b| *b == b':').count() > 1
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-:".contains(&b))
            })
        {
            return base;
        }
        for port in operands.iter().skip(1).take(3) {
            label.push(' ');
            label.push_str(&short(port, 20));
        }
        if operands.len() > 4 {
            label.push_str(&format!(" +{} ports", operands.len() - 4));
        }
    } else if let Some(container) = container {
        label.push_str(&format!(" container={}", short(container, 32)));
    }
    short(&label, 159)
}
// Each poll writes at most one bounded chunk. Queue advancement occurs in the
// same poll as write(), so cancelling next() cannot replay or lose written bytes.
async fn flush_input(master: &AsyncFd<OwnedFd>, queue: &mut VecDeque<u8>) -> io::Result<()> {
    let mut ready = master.writable().await?;
    let result = ready.try_io(|fd| {
        let bytes = queue.as_slices().0;
        let count =
            unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len().min(8192)) };
        if count < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(count as usize)
        }
    });
    match result {
        Ok(Ok(0)) => Err(io::ErrorKind::WriteZero.into()),
        Ok(Ok(count)) => {
            queue.drain(..count);
            Ok(())
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(()),
    }
}

// Keep the leader unreaped until its owned process group has been stopped;
// otherwise a reused PID could refer to an unrelated process during cleanup.
fn has_exited(pid: libc::pid_t) -> io::Result<bool> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        return Ok(unsafe { info.si_pid() } == pid);
    }
}
fn kill_owned_group(pid: libc::pid_t) -> io::Result<()> {
    // The unreaped direct child reserves this PID/PGID throughout both calls.
    if unsafe { libc::kill(pid, libc::SIGKILL) } < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    #[cfg(target_os = "macos")]
    if error.raw_os_error() == Some(libc::EPERM) && has_exited(pid)? {
        let mut members = [0 as libc::pid_t; 2];
        let count = unsafe {
            libc::proc_listpgrppids(
                pid,
                members.as_mut_ptr().cast(),
                std::mem::size_of_val(&members) as libc::c_int,
            )
        };
        if count == 1 && members[0] == pid {
            return Ok(());
        }
    }
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}
async fn wait_owned(
    child: &mut tokio::process::Child,
    pid: libc::pid_t,
    exited: &mut tokio::signal::unix::Signal,
) -> io::Result<std::process::ExitStatus> {
    while !has_exited(pid)? {
        exited
            .recv()
            .await
            .ok_or_else(|| io::Error::other("PTY child exit stream closed"))?;
    }
    kill_owned_group(pid)?;
    child.wait().await
}
impl Drop for Session {
    fn drop(&mut self) {
        self.reader.abort();
        if !self.armed {
            return;
        }
        if let Err(error) = kill_owned_group(self.pid) {
            eprintln!("hamn: cannot terminate terminal process group: {error}");
        }
        loop {
            if unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), 0) } >= 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                eprintln!("hamn: cannot reap terminal child: {error}");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_labels_identify_resource_ports_and_container_without_credentials() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = crate::preferences::Workspace::Kubernetes;
        for (input, expected) in [
            (
                "port-forward --token token-secret --namespace team pod/api 8080:80 9090:http",
                "kubectl port-forward pod/api 8080:80 9090:http",
            ),
            (
                "logs --token --container=token-secret pod/api -c sidecar --password password-secret --tail=200",
                "kubectl logs pod/api container=sidecar",
            ),
            (
                "logs -f --container=app pod/api --client-key /private/key",
                "kubectl logs pod/api container=app",
            ),
            ("logs pod/api --future-value secret-value", "kubectl logs"),
            ("exec pod/api -- sh -c secret-value", "kubectl exec"),
            (
                "port-forward pod/api -- :http",
                "kubectl port-forward pod/api :http",
            ),
        ] {
            let invocation = crate::native::parse(input, &state).unwrap();
            let original = invocation.args.clone();
            assert_eq!(session_summary(&invocation), expected, "{input}");
            assert_eq!(invocation.args, original, "labels must not rewrite argv");
        }
        state.workspace = crate::preferences::Workspace::Containers;
        for input in [
            "docker --context demo logs --tail 200 --since hidden-value web",
            "container logs -ft web",
        ] {
            assert_eq!(
                session_summary(&crate::native::parse(input, &state).unwrap()),
                "docker logs web"
            );
        }
    }
    #[test]
    fn session_labels_bound_untrusted_operands_and_omit_unsafe_values() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = crate::preferences::Workspace::Kubernetes;
        let query = crate::native::parse(
            &format!(
                "port-forward pod/{} {}",
                "a".repeat(1024),
                vec!["8080:80"; 20].join(" ")
            ),
            &state,
        )
        .unwrap();
        let label = session_summary(&query);
        assert!(label.chars().count() <= 160, "{label}");
        assert!(
            label.contains("8080:80") && label.contains("+17 ports"),
            "{label}"
        );
        for input in [
            "logs 'pod/secret@example'",
            "port-forward pod/api '80:secret@example'",
            "logs pod/api --token",
            "logs -t secret-value",
        ] {
            let invocation = crate::native::parse(input, &state).unwrap();
            let label = session_summary(&invocation);
            assert!(
                !label.contains("secret") && !label.contains("pod/"),
                "{label}"
            );
        }
    }
    #[tokio::test]
    async fn scrollback_exposes_history_without_consuming_live_cli_keys() {
        let invocation =
            crate::native::parse("version", &crate::tui_state::State::new(Default::default()))
                .unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "i=0; while test $i -lt 40; do printf 'history-%02d\\n' $i; i=$((i+1)); done; printf INPUT_READY; read value; printf '\\nreceived:%s\\n' \"$value\";"]);
        let mut session = Session::spawn(command, invocation, 60, 12).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !session.parser.screen().contents().contains("INPUT_READY") {
                if let Event::Output(bytes) = session.next().await.unwrap() {
                    session.parser.process(&bytes);
                }
            }
            let page_up = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
            assert!(!session.scroll_key(page_up)); // A running pager still receives its own PgUp.
            assert!(!session.scroll_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT)));
            let shifted = KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT);
            for _ in 0..10 {
                assert!(session.scroll_key(shifted));
            }
            assert!(session.parser.screen().contents().contains("history-00"));
            assert!(session.parser.screen().scrollback() > 0);
            session.input(b"accepted\r").unwrap();
            assert_eq!(session.parser.screen().scrollback(), 0);
            loop {
                match session.next().await.unwrap() {
                    Event::Output(bytes) => session.parser.process(&bytes),
                    Event::Exited(code) => {
                        assert_eq!(code, 0);
                        if session.ended {
                            break;
                        }
                    }
                    Event::Ended if session.exit.is_some() => break,
                    Event::Ended | Event::InputProgress => {}
                }
            }
            assert!(
                session
                    .parser
                    .screen()
                    .contents()
                    .contains("received:accepted")
            );
            for _ in 0..10 {
                assert!(session.scroll_key(page_up));
            }
            assert!(session.parser.screen().contents().contains("history-00"));
            for _ in 0..10 {
                assert!(session.scroll_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)));
            }
            assert_eq!(session.parser.screen().scrollback(), 0);
            assert!(
                session
                    .parser
                    .screen()
                    .contents()
                    .contains("received:accepted")
            );
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn real_pty_preserves_input_resize_stderr_and_exit_status() {
        let state = crate::tui_state::State::new(Default::default());
        let invocation = crate::native::parse("version", &state).unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "test -t 0 && test -t 1 && test -t 2 || exit 90; printf ready; read value; printf 'input:%s\n' \"$value\"; stty size; printf stderr-marker >&2; exit 7"]);
        let mut session = Session::spawn(command, invocation, 80, 24).unwrap();
        let mut output = Vec::new();
        let mut sent = false;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                match session.next().await.unwrap() {
                    Event::Output(bytes) => {
                        output.extend(bytes);
                        if !sent && String::from_utf8_lossy(&output).contains("ready") {
                            session.resize(100, 33).unwrap();
                            session.write(b"hello\r").unwrap();
                            sent = true;
                        }
                    }
                    Event::Exited(code) => {
                        assert_eq!(code, 7);
                        if session.ended {
                            break;
                        }
                    }
                    Event::Ended if session.exit.is_some() => break,
                    Event::Ended | Event::InputProgress => {}
                }
            }
        })
        .await
        .unwrap();
        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("input:hello")
                && text.contains("30 100")
                && text.contains("stderr-marker"),
            "{text}"
        );
    }
}

#[cfg(test)]
#[path = "terminal_input_tests.rs"]
mod input_tests;
