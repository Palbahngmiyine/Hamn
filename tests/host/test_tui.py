#!/usr/bin/env python3
"""PTY tests use output readiness, not sleeps, and always reap the whole session."""
import errno
import fcntl
import json
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


def terminal_settings(fd):
    value = termios.tcgetattr(fd)
    # Darwin termios(4) defines PENDIN as pending-input state, not a mode.
    # Compare every user-controlled mode bit and control character exactly.
    value[3] &= ~getattr(termios, 'PENDIN', 0)
    return value


def exercise(exit_mode, command=None):
    with tempfile.TemporaryDirectory(prefix="hamn-tui-") as directory:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        before = terminal_settings(slave)
        process = subprocess.Popen(command or [binary], stdin=slave, stdout=slave, stderr=slave,
                                   env=dict(os.environ, HOME=directory, TERM="xterm-256color"),
                                   start_new_session=True)
        output = bytearray()

        def until(marker):
            deadline = time.monotonic() + 10
            while marker not in output:
                remaining = deadline - time.monotonic()
                assert remaining > 0, (exit_mode, bytes(output))
                ready, _, _ = select.select([master], [], [], remaining)
                assert ready, (exit_mode, "TUI output deadline exceeded", bytes(output[-5000:]))
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
            elif exit_mode == "confirm-small":
                os.write(master, b":vm create --profile work\r")
                until(b"Impact:")
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 8, 30, 0, 0))
                os.kill(process.pid, signal.SIGWINCH)
                until(b"disabled.")
                os.write(master, b"yn")
                until(b"selected")  # cancellation has returned to the small-screen warning
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
                os.kill(process.pid, signal.SIGWINCH)
                os.write(master, b":contexts\r")
                until(b"contexts")
                assert not (Path(directory) / ".hamn").exists(), "hidden confirmation executed a mutation"
                os.write(master, b"q")
            elif exit_mode == "interrupt":
                os.write(master, b"\x03")
            elif exit_mode.startswith("suspend"):
                if exit_mode == "suspend-key":
                    os.write(master, b"\x1a")
                else:
                    os.kill(process.pid, signal.SIGTSTP)
                until(b"\x1b[?1049l")
                assert terminal_settings(slave) == before
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
                assert terminal_settings(slave) != before
                os.write(master, b"q")
            elif exit_mode == "panic":
                pass  # the isolated Rust test intentionally unwinds after drawing
            else:
                os.kill(process.pid, signal.SIGTERM)
            until(b"\x1b[?1049l")
            assert process.wait(timeout=5) == (101 if exit_mode == "panic" else 0)
            assert terminal_settings(slave) == before, ("terminal settings were not restored", before, terminal_settings(slave), bytes(output[-3000:]))
            assert not (Path(directory) / ".hamn").exists(), "TUI observation changed profile state"
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
            os.close(master)
            os.close(slave)


for mode in ("q", "confirm-small", "interrupt", "terminate", "suspend-key", "suspend-signal"):
    exercise(mode)
build = subprocess.run(['cargo', 'test', '--locked', '--no-run', '--message-format=json'],
                       capture_output=True, text=True, check=True, timeout=180)
artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith('{')]
test_binary = next(item['executable'] for item in artifacts if item.get('reason') == 'compiler-artifact'
                   and item.get('profile', {}).get('test') and item.get('executable'))
exercise('panic', [test_binary, 'tui::tests::panic_restores_terminal_fixture', '--ignored', '--exact', '--nocapture'])
print("TUI entry, navigation, resize and terminal restoration: passed")
