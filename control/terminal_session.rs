use crate::{native::Invocation, terminal_io::{self, Replies}};
use std::{io, os::fd::{AsRawFd, FromRawFd, OwnedFd}, process::Stdio, sync::Arc};
use tokio::{io::unix::AsyncFd, sync::mpsc};
use ratatui::{layout::{Constraint, Layout}, widgets::Paragraph};

pub enum Event { Output(Vec<u8>), Exited(i32), Ended }
pub struct Session {
    pub invocation: Invocation,
    pub parser: vt100::Parser<Replies>,
    pub exit: Option<i32>,
    child: tokio::process::Child,
    master: Arc<AsyncFd<OwnedFd>>,
    output: mpsc::Receiver<io::Result<Vec<u8>>>,
    reader: tokio::task::JoinHandle<()>,
    ended: bool,
}
impl Session {
    pub fn start(invocation: Invocation, width: u16, height: u16) -> io::Result<Self> {
        let mut command = invocation.command(false);
        let _input = invocation.body.as_deref().map(|body| crate::command_input::attach(&mut command, body)).transpose()?;
        Self::spawn(command, invocation, width, height)
    }
    fn spawn(mut command: tokio::process::Command, invocation: Invocation, width: u16, height: u16) -> io::Result<Self> {
        let (rows, cols) = (height.saturating_sub(3).max(1), width.max(1));
        let mut size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        let (mut master, mut slave) = (-1, -1);
        if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) } != 0 { return Err(io::Error::last_os_error()); }
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 { return Err(io::Error::last_os_error()); }
        }
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } != 0 { return Err(io::Error::last_os_error()); }
        command.stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave));
        command.env("TERM", "xterm-256color").kill_on_drop(true);
        unsafe { command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 { return Err(io::Error::last_os_error()); }
            Ok(())
        }); }
        let child = command.spawn()?;
        let master = Arc::new(AsyncFd::new(master)?);
        let source = master.clone();
        let (sender, output) = mpsc::channel(64);
        let reader = tokio::spawn(async move {
            loop {
                let result = async {
                    let mut ready = source.readable().await?;
                    let mut bytes = vec![0; 8192];
                    let read = ready.try_io(|fd| {
                        let count = unsafe { libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
                        if count < 0 { Err(io::Error::last_os_error()) } else { Ok(count as usize) }
                    });
                    match read { Ok(Ok(n)) => { bytes.truncate(n); Ok(Some(bytes)) }, Ok(Err(e)) => Err(e), Err(_) => Ok(None) }
                }.await;
                match result {
                    Ok(Some(bytes)) if bytes.is_empty() => return,
                    Ok(Some(bytes)) => if sender.send(Ok(bytes)).await.is_err() { return; },
                    Ok(None) => {},
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => return,
                    Err(e) => { let _ = sender.send(Err(e)).await; return; },
                }
            }
        });
        Ok(Self { invocation, parser: vt100::Parser::new_with_callbacks(rows, cols, 10000, Replies::default()),
            exit: None, child, master, output, reader, ended: false })
    }
    pub async fn next(&mut self) -> io::Result<Event> {
        tokio::select! {
            biased;
            result = self.output.recv(), if !self.ended => match result {
                Some(bytes) => Ok(Event::Output(bytes?)),
                None => { self.ended = true; Ok(Event::Ended) },
            },
            result = self.child.wait(), if self.exit.is_none() => {
                use std::os::unix::process::ExitStatusExt;
                let status = result?;
                let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                self.exit = Some(code); Ok(Event::Exited(code))
            },
            else => std::future::pending().await,
        }
    }
    pub async fn write(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let mut ready = self.master.writable().await?;
            match ready.try_io(|fd| {
                let count = unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
                if count < 0 { Err(io::Error::last_os_error()) } else { Ok(count as usize) }
            }) {
                Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(Ok(n)) => bytes = &bytes[n..], Ok(Err(e)) => return Err(e), Err(_) => {},
            }
        }
        Ok(())
    }
    pub fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        let (rows, cols) = (height.saturating_sub(3).max(1), width.max(1));
        let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 { return Err(io::Error::last_os_error()); }
        self.parser.screen_mut().set_size(rows, cols); Ok(())
    }
    pub fn signal(&mut self, signal: i32) -> io::Result<()> {
        if self.exit.is_none() && self.child.try_wait()?.is_none() {
            if let Some(pid) = self.child.id() {
                if unsafe { libc::kill(-(pid as i32), signal) } < 0 { return Err(io::Error::last_os_error()); }
            }
        }
        Ok(())
    }
    pub fn draw(&self, frame: &mut ratatui::Frame) {
        let areas = Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
        frame.render_widget(Paragraph::new(crate::tui_state::clean(&format!("Hamn | {} terminal\n{}", self.invocation.program(), self.invocation.target))), areas[0]);
        frame.render_widget(terminal_io::Screen(self.parser.screen()), areas[1]);
        let status = self.exit.map(|code| format!("Exit code {code} | Enter / Esc returns to the resource list"))
            .unwrap_or_else(|| "Input goes to CLI | Ctrl-C interrupts | Docker Ctrl-P Ctrl-Q detaches".into());
        frame.render_widget(Paragraph::new(status), areas[2]);
        if !self.parser.screen().hide_cursor() && self.exit.is_none() {
            let (row, col) = self.parser.screen().cursor_position();
            frame.set_cursor_position((areas[1].x + col, areas[1].y + row));
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.signal(libc::SIGHUP);
        self.reader.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_pty_preserves_input_resize_stderr_and_exit_status() {
        let state = crate::tui_state::State::new(Default::default());
        let invocation = crate::native::parse("version", &state).unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "test -t 0 && test -t 1 && test -t 2 || exit 90; printf ready; read value; printf 'input:%s\n' \"$value\"; stty size; printf stderr-marker >&2; exit 7"]);
        let mut session = Session::spawn(command, invocation, 80, 24).unwrap();
        let mut output = Vec::new(); let mut sent = false;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                match session.next().await.unwrap() {
                    Event::Output(bytes) => {
                        output.extend(bytes);
                        if !sent && String::from_utf8_lossy(&output).contains("ready") {
                            session.resize(100, 33).unwrap(); session.write(b"hello\r").await.unwrap(); sent = true;
                        }
                    },
                    Event::Exited(code) => { assert_eq!(code, 7); if session.ended { break; } },
                    Event::Ended if session.exit.is_some() => break,
                    Event::Ended => {},
                }
            }
        }).await.unwrap();
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("input:hello") && text.contains("30 100") && text.contains("stderr-marker"), "{text}");
    }
}
