#!/usr/bin/env python3
"""VM stop remains interactive and completes with an unresponsive SSH socket."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
from terminal_screen import Screen

binary = Path(os.environ.get('HAMN', 'target/debug/hamn')).resolve()
with tempfile.TemporaryDirectory(prefix='hamn-tui-ssh-', dir='/tmp') as directory:
    env = dict(os.environ, HOME=directory, PATH='/usr/bin:/bin', TERM='xterm-256color')
    created = subprocess.run([binary, '--headless', 'vm', 'create', '--profile', 'test', '--yes'],
        env=env, capture_output=True, timeout=10, check=True)
    assert json.loads(created.stdout)['ok']
    server = socket.socket(socket.AF_UNIX)
    server.bind(str(Path(directory) / '.hamn/test/ssh.sock'))
    server.listen(1)
    server.settimeout(10)
    accepted, release = threading.Event(), threading.Event()
    notice_read, notice_write = os.pipe()
    errors = []

    def serve():
        try:
            with server.accept()[0]:
                accepted.set()
                os.write(notice_write, b'1')
                assert release.wait(15)
        except Exception as error:
            errors.append(error)
            os.write(notice_write, b'0')

    thread = threading.Thread(target=serve)
    thread.start()
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 140, 0, 0))
    original = termios.tcgetattr(slave)
    child = subprocess.Popen([binary], env=env, stdin=slave, stdout=slave, stderr=slave,
                             start_new_session=True)
    output = bytearray()
    screen = Screen(40, 160)
    ansi = re.compile(rb'\x1b\[[0-?]*[ -/]*[@-~]')

    def until(marker, timeout=10):
        deadline = time.monotonic() + timeout
        while marker not in re.sub(rb'\s+', b'', screen.text().encode()):
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([master], [], [], remaining)[0]:
                raise AssertionError((marker, bytes(output[-2000:])))
            data = os.read(master, 65536)
            output.extend(data); screen.feed(data)

    try:
        until(b'Hamn')
        os.write(master, b'1\r')
        until(b'[Containers]')
        os.write(master, b':vm stop --profile test\r')
        until(b'Impact:')
        output.clear()
        os.write(master, b'y')
        # Continue draining redraws: waiting only on the server can fill the PTY
        # and prevent the frontend from consuming confirmation or starting work.
        deadline = time.monotonic() + 5
        while not accepted.is_set():
            remaining = deadline - time.monotonic()
            assert remaining > 0, ('VM stop did not attempt SSH cleanup', errors, bytes(output[-4000:]))
            ready = select.select([master, notice_read], [], [], remaining)[0]
            if master in ready:
                data = os.read(master, 65536)
                output.extend(data); screen.feed(data)
            if notice_read in ready:
                assert os.read(notice_read, 1) == b'1', (errors, bytes(output[-4000:]))
        # Input must remain responsive while the C worker waits on SSH.
        os.write(master, b'?')
        until(b'Commands:', timeout=2)
        until(b'Operationcompleted')
        # A background completion must not replace the help/navigation screen.
        status = subprocess.run([binary, '--headless', 'vm', 'status', '--profile', 'test'],
            env=env, capture_output=True, timeout=10, check=True)
        snapshot = json.loads(status.stdout)['data']
        assert snapshot['state'] == 'stopped', snapshot
        assert snapshot['lastOperation']['status'] == 'completed', snapshot
        os.write(master, b'q')
        assert child.wait(timeout=5) == 0
        restored = termios.tcgetattr(slave)
        original[3] &= ~getattr(termios, 'PENDIN', 0)
        restored[3] &= ~getattr(termios, 'PENDIN', 0)
        assert original == restored
        assert not (Path(directory) / '.hamn/test/ssh.sock').exists()
    finally:
        release.set()
        thread.join(timeout=15)
        server.close()
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)
        os.close(master)
        os.close(slave)
        os.close(notice_read)
        os.close(notice_write)
    assert not errors and not thread.is_alive(), errors
print('PASS: TUI remains interactive and stops with a stalled SSH control socket')
