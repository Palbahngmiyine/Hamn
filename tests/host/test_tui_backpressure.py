#!/usr/bin/env python3
"""Whole-TUI PTY backpressure checks; no VM, network or user configuration."""
import fcntl
import hashlib
import os
import signal
import struct
import sys
import termios
import threading
from test_tui_native_regressions import Harness

PASTE = b'x' * (512 * 1024)
FIXTURE = r'''
import hashlib, json, os, signal, sys, tty
from pathlib import Path
root = Path(os.environ['FIXTURE_ROOT'])
if 'stdin-proof' not in sys.argv:
    print(json.dumps({'ID': 'abc123', 'Names': 'old-target-row', 'State': 'running'}))
    sys.exit(0)
tty.setraw(0)
def interrupted(number, *_):
    print('CLI_INTERRUPTED', flush=True)
    sys.exit(128 + number)
signal.signal(signal.SIGINT, interrupted)
signal.signal(signal.SIGTERM, interrupted)
signal.signal(signal.SIGWINCH, lambda *_: print('RESIZED:%dx%d' % os.get_terminal_size(0), flush=True))
print('INPUT_READY', flush=True)
with (root / 'gate').open() as gate:
    gate.read(1)
if 'ordered' in sys.argv:
    data = bytearray()
    while len(data) < 512 * 1024 + 5:
        data.extend(os.read(0, 512 * 1024 + 5 - len(data)))
    print('DIGEST:' + hashlib.sha256(data).hexdigest(), flush=True)
else:
    print('RELEASED', flush=True)
'''


def deliver(harness, payload):
    """Notify the same bounded select loop when the outer paste is fully sent."""
    errors = []
    def send():
        try:
            remaining = payload
            while remaining:
                count = os.write(harness.master, remaining)
                remaining = remaining[count:]
            os.write(harness.notice, b'paste-delivered\n')
        except OSError as error:
            errors.append(error)
    thread = threading.Thread(target=send, daemon=True)
    thread.start()
    return thread, errors


def exercise(mode):
    harness = Harness('containers')
    sender = None
    waiter = None
    try:
        harness.until('old-target-row')
        cli = harness.root / 'bin/docker'
        cli.write_text(f'#!{sys.executable}\n' + FIXTURE)
        cli.chmod(0o755)
        harness.send(f':stdin-proof {mode}\r'.encode(), 'INPUT_READY')
        tail = b'END\x10\x11' if mode == 'ordered' else b'\x03'
        sender, errors = deliver(harness, b'\x1b[200~' + PASTE + b'\x1b[201~' + tail)
        harness.wait(lambda: b'paste-delivered' in harness.notices)
        sender.join(timeout=1)
        assert not sender.is_alive() and not errors, errors
        if mode == 'ordered':
            os.write(harness.gate, b'1')
            harness.until('DIGEST:' + hashlib.sha256(PASTE + tail).hexdigest())
            harness.until('Exit code 0')
        else:
            # A raw CLI does not interpret Ctrl-C as a signal. The queued byte
            # must not become an unrequested out-of-band interrupt.
            fcntl.ioctl(harness.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 150, 0, 0))
            os.kill(harness.child.pid, signal.SIGWINCH)
            harness.until('RESIZED:150x29')
            assert 'CLI_INTERRUPTED' not in harness.screen.text()
            if mode == 'terminate':
                def exited():
                    harness.child.wait(timeout=10)
                    os.write(harness.notice, b'frontend-exited\n')
                waiter = threading.Thread(target=exited, daemon=True)
                waiter.start()
                os.kill(harness.child.pid, signal.SIGTERM)
                harness.wait(lambda: b'frontend-exited' in harness.notices)
                waiter.join(timeout=1)
                assert harness.child.returncode == 0 and not waiter.is_alive()
                return
            harness.send(b'\x1b\x03', 'Exit code 130')  # Ctrl+Alt+C
            assert 'discarded' in harness.screen.text(), harness.screen.text()
        harness.send(b'\r', '> abc123')
        assert 'old-target-row' in harness.screen.text()
    finally:
        # This also releases an old-binary reproduction that remains blocked.
        os.write(harness.gate, b'1')
        harness.close()
        for thread in (sender, waiter):
            if thread:
                thread.join(timeout=2)
                assert not thread.is_alive(), 'fixture thread was not cleaned up'


if __name__ == '__main__':
    for mode in ('terminate', 'interrupt', 'ordered'):
        exercise(mode)
    print('PASS: queued paste preserves bytes and leaves resize, output, termination and explicit interrupt responsive')
