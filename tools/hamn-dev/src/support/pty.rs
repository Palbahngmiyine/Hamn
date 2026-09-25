//! Pseudo-terminals, FIFOs and bounded descriptor waits for process tests.
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A master/slave pair whose window is `rows` x `cols`.
pub struct Pty {
    pub master: OwnedFd,
    pub slave: OwnedFd,
}

impl Pty {
    pub fn open(rows: u16, cols: u16) -> Self {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty writes two descriptors; the name, termios and
        // window arguments may be null.
        let result = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
        };
        assert_eq!(result, 0, "openpty: {}", io::Error::last_os_error());
        // openpty's descriptors are inheritable; Python's are not (PEP 446).
        // Children get the slave only as their dup2'd standard streams.
        for fd in [master, slave] {
            // SAFETY: F_SETFD only changes the flags of a descriptor owned here.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, 0, "fcntl: {}", io::Error::last_os_error());
        }
        // SAFETY: openpty returned two new descriptors owned by nobody else.
        let pty = unsafe { Self { master: OwnedFd::from_raw_fd(master), slave: OwnedFd::from_raw_fd(slave) } };
        pty.resize(rows, cols);
        pty
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ reads one winsize from the pointer.
        let result = unsafe { libc::ioctl(self.slave.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        assert_eq!(result, 0, "TIOCSWINSZ: {}", io::Error::last_os_error());
    }

    /// Starts `command` in a new session with the slave as its standard
    /// streams (no controlling terminal is acquired, as with Python's
    /// `start_new_session=True`).
    pub fn spawn(&self, command: &mut Command) -> Child {
        let stream = || Stdio::from(self.slave.try_clone().expect("duplicate PTY slave"));
        command.stdin(stream()).stdout(stream()).stderr(stream());
        // SAFETY: setsid is async-signal-safe and touches no parent state.
        unsafe {
            command.pre_exec(|| if libc::setsid() < 0 { Err(io::Error::last_os_error()) } else { Ok(()) });
        }
        command.spawn().expect("spawn under PTY")
    }
}

/// Creates a FIFO and opens it read-write and non-blocking, so it neither
/// blocks the test nor reports end-of-file while no fixture holds it.
pub fn fifo(path: &Path) -> OwnedFd {
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is a valid C string.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo {}: {}", path.display(), io::Error::last_os_error());
    // SAFETY: as above; open returns a new descriptor or -1.
    let fd = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    assert!(fd >= 0, "open {}: {}", path.display(), io::Error::last_os_error());
    // SAFETY: fd is a new descriptor owned here.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// Waits up to `timeout` for any of `fds` to become readable and returns
/// the readable ones (empty on timeout).
pub fn readable(fds: &[RawFd], timeout: Duration) -> Vec<RawFd> {
    let mut polls: Vec<libc::pollfd> = fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    loop {
        // SAFETY: polls holds fds.len() initialized entries.
        let result = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, millis) };
        if result >= 0 {
            break;
        }
        let error = io::Error::last_os_error();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted, "poll: {error}");
    }
    polls.iter().filter(|poll| poll.revents != 0).map(|poll| poll.fd).collect()
}

/// Reads what is available from `fd` (up to 64 KiB). An empty result means
/// end-of-file; a PTY master whose slave closed reports EIO, also returned
/// as empty.
pub fn read_some(fd: RawFd) -> Vec<u8> {
    let mut buffer = vec![0u8; 65536];
    loop {
        // SAFETY: buffer is writable for its length.
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count >= 0 {
            buffer.truncate(count as usize);
            return buffer;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EIO) => return Vec::new(),
            Some(libc::EAGAIN) => return Vec::new(),
            _ => panic!("read: {error}"),
        }
    }
}

/// Writes all of `data` to `fd`, retrying partial writes.
pub fn write_all(fd: RawFd, data: &[u8]) {
    // SAFETY: the File borrows fd only for this call and is not dropped.
    let mut file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    file.write_all(data).expect("write");
}

/// Reads until end-of-file from a file path (for recorded fixture output).
pub fn read_file(path: &Path) -> String {
    let mut text = String::new();
    File::open(path).and_then(|mut file| file.read_to_string(&mut text)).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text
}

/// Waits up to `timeout` for `child` to exit while draining `master`, so a
/// full PTY cannot block the child's final redraw and exit.
pub fn wait_for_exit(child: &mut Child, master: RawFd, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            return Some(status);
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        if !readable(&[master], remaining.min(Duration::from_millis(100))).is_empty() {
            read_some(master);
        }
    }
}

/// Sends `signal` to the process `pid`.
pub fn kill(pid: u32, signal: i32) {
    // SAFETY: kill only sends a signal.
    unsafe { libc::kill(pid as i32, signal) };
}

/// Sends `signal` to the process group `pid` (a session leader's group).
pub fn kill_group(pid: u32, signal: i32) {
    // SAFETY: killpg only sends a signal.
    unsafe { libc::killpg(pid as i32, signal) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Like Python's non-inheritable descriptors (PEP 446), the PTY and FIFO
    /// descriptors stay with the test: a child gets only its standard
    /// streams, so no process under test holds the master or a FIFO.
    #[test]
    fn children_inherit_only_their_standard_streams() {
        let directory = crate::support::tmp::TempDir::new("hamn-dev-pty-");
        let notice = fifo(&directory.path().join("notice"));
        let pty = Pty::open(24, 80);
        let fds = [pty.master.as_raw_fd(), pty.slave.as_raw_fd(), notice.as_raw_fd()];
        // An external test(1) sees the shell's inherited descriptors in
        // /dev/fd. A shell redirection would not do: the shell saves the
        // descriptors it redirects on numbers from 10 up.
        let script = format!(
            "for fd in {} {} {}; do if /bin/test -e /dev/fd/$fd; then echo inherited-$fd; fi; done; echo checked",
            fds[0], fds[1], fds[2]
        );
        let mut child = pty.spawn(Command::new("/bin/sh").args(["-c", &script]));
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !output.windows(7).any(|window| window == b"checked") {
            let remaining = deadline.checked_duration_since(Instant::now()).expect("child output deadline");
            if !readable(&[pty.master.as_raw_fd()], remaining).is_empty() {
                output.extend(read_some(pty.master.as_raw_fd()));
            }
        }
        assert!(child.wait().unwrap().success());
        let output = String::from_utf8_lossy(&output);
        assert!(!output.contains("inherited"), "{fds:?}: {output}");
    }
}
