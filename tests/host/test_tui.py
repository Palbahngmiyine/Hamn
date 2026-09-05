#!/usr/bin/env python3
"""PTY tests use output readiness, not sleeps, and always reap the whole session."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time

binary = Path(os.environ.get("HAMN", "target/debug/hamn")).resolve()


def exercise(exit_mode):
    with tempfile.TemporaryDirectory(prefix="hamn-tui-") as directory:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        before = termios.tcgetattr(slave)
        process = subprocess.Popen([binary], stdin=slave, stdout=slave, stderr=slave,
                                   env=dict(os.environ, HOME=directory, TERM="xterm-256color"),
                                   start_new_session=True)
        output = bytearray()

        def until(marker):
            deadline = time.monotonic() + 10
            while marker not in output:
                remaining = deadline - time.monotonic()
                assert remaining > 0, (exit_mode, bytes(output))
                ready, _, _ = select.select([master], [], [], remaining)
                assert ready, "TUI output deadline exceeded"
                output.extend(os.read(master, 65536))

        try:
            until(b"Hamn")
            assert b"\x1b[?1049h" in output
            if exit_mode == "q":
                os.write(master, b":contexts\r")
                until(b"k8s contexts list")
                os.write(master, "/작업".encode())
                until("작".encode())
                until("업".encode())  # incremental frames put CSI codes between characters
                os.write(master, b"\r")
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 10, 30, 0, 0))
                os.kill(process.pid, signal.SIGWINCH)
                os.write(master, b"q")
            elif exit_mode == "interrupt":
                os.write(master, b"\x03")
            elif exit_mode.startswith("suspend"):
                if exit_mode == "suspend-key":
                    os.write(master, b"\x1a")
                else:
                    os.kill(process.pid, signal.SIGTSTP)
                until(b"\x1b[?1049l")
                assert termios.tcgetattr(slave) == before
                def deadline(*_args):
                    raise TimeoutError("TUI did not stop after restoring the terminal")
                old_alarm = signal.signal(signal.SIGALRM, deadline)
                signal.alarm(5)
                try:
                    _, status = os.waitpid(process.pid, os.WUNTRACED)
                    assert os.WIFSTOPPED(status)
                finally:
                    signal.alarm(0)
                    signal.signal(signal.SIGALRM, old_alarm)
                output.clear()
                os.kill(process.pid, signal.SIGCONT)
                until(b"Hamn")
                assert termios.tcgetattr(slave) != before
                os.write(master, b"q")
            else:
                os.kill(process.pid, signal.SIGTERM)
            until(b"\x1b[?1049l")
            assert process.wait(timeout=5) == 0
            assert termios.tcgetattr(slave) == before, "terminal settings were not restored"
            assert not (Path(directory) / ".hamn").exists(), "TUI observation changed profile state"
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
            os.close(master)
            os.close(slave)


for mode in ("q", "interrupt", "terminate", "suspend-key", "suspend-signal"):
    exercise(mode)
print("TUI entry, navigation, resize and terminal restoration: passed")
