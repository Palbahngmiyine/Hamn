//! Terminal mode snapshots, for PTY tests that check a program restored its
//! terminal, and the raw mode that fixtures reading keystrokes select.
use std::io;
use std::os::fd::RawFd;

/// A terminal's modes, speeds and control characters (Python's
/// `termios.tcgetattr` list), except PENDIN: Darwin termios(4) defines
/// PENDIN as pending-input state, not a mode, so it is cleared. Every
/// user-controlled mode bit and control character compares exactly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    pub iflag: libc::tcflag_t,
    pub oflag: libc::tcflag_t,
    pub cflag: libc::tcflag_t,
    pub lflag: libc::tcflag_t,
    pub ispeed: libc::speed_t,
    pub ospeed: libc::speed_t,
    pub cc: [libc::cc_t; libc::NCCS],
}

/// The settings of the terminal open on `fd`.
pub fn settings(fd: RawFd) -> Settings {
    let mode = get(fd).unwrap_or_else(|error| panic!("tcgetattr {fd}: {error}"));
    Settings {
        iflag: mode.c_iflag,
        oflag: mode.c_oflag,
        cflag: mode.c_cflag,
        lflag: mode.c_lflag & !libc::PENDIN,
        ispeed: mode.c_ispeed,
        ospeed: mode.c_ospeed,
        cc: mode.c_cc,
    }
}

fn get(fd: RawFd) -> io::Result<libc::termios> {
    // SAFETY: termios is plain data; tcgetattr fills it or fails.
    let mut mode: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: mode is a valid, writable termios.
    if unsafe { libc::tcgetattr(fd, &mut mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}

/// Python 3.9's `tty.setraw(fd)` (the fixtures' former /usr/bin/python3):
/// no input translation, flow control, output processing, echo, canonical
/// input or signal keys; eight-bit characters; reads return each byte.
pub fn set_raw(fd: RawFd) -> io::Result<()> {
    let mut mode = get(fd)?;
    mode.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
    mode.c_oflag &= !libc::OPOST;
    mode.c_cflag &= !(libc::CSIZE | libc::PARENB);
    mode.c_cflag |= libc::CS8;
    mode.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
    mode.c_cc[libc::VMIN] = 1;
    mode.c_cc[libc::VTIME] = 0;
    // SAFETY: mode is a valid termios read from the same terminal.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::pty::Pty;
    use std::os::fd::AsRawFd;

    #[test]
    fn raw_mode_changes_settings_and_pending_input_is_ignored() {
        let pty = Pty::open(24, 80);
        let fd = pty.slave.as_raw_fd();
        let before = settings(fd);
        assert_ne!(before.lflag & libc::ICANON, 0, "a new PTY starts canonical");
        set_raw(fd).unwrap();
        let after = settings(fd);
        assert_ne!(before, after);
        assert_eq!(after.lflag & (libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG), 0);
        assert_eq!(after.iflag & libc::IXON, 0);
        assert_eq!((after.cc[libc::VMIN], after.cc[libc::VTIME]), (1, 0));
        // PENDIN is state, not a mode: setting it changes nothing compared.
        let mut mode = get(fd).unwrap();
        mode.c_lflag |= libc::PENDIN;
        // SAFETY: mode is a valid termios read from the same terminal.
        assert_eq!(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &mode) }, 0);
        assert_eq!(settings(fd), after);
    }
}
