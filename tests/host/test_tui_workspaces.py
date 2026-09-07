#!/usr/bin/env python3
"""Disposable PTY contract tests: selection, CLI dispatch, interactive input, restoration."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
from terminal_screen import RatatuiScreen as Screen, wait_for_exit

binary = Path(os.environ.get('HAMN', 'target/debug/hamn')).resolve()
with tempfile.TemporaryDirectory(prefix='hamn-workspaces-', dir='/tmp') as directory:
    root = Path(directory)
    tools = root / 'bin'
    tools.mkdir()
    fixture = '''#!/usr/bin/python3
import json, os, sys, tty
args = sys.argv[1:]
with open(os.environ['CLI_RECORD'], 'a') as out: out.write(json.dumps([os.path.basename(sys.argv[0]), args]) + '\\n')
if 'ps' in args:
    if '-q' in args: print('RAW_OUTPUT'); sys.exit(7)
    print(json.dumps({'ID':'abc123','Names':'fixture-container','State':'running'}))
elif 'exec' in args:
    assert os.isatty(0) and os.isatty(1) and os.isatty(2)
    tty.setraw(0)
    print('INPUT_READY', flush=True)
    data = b''
    while len(data) < 2: data += os.read(0, 2 - len(data))
    print('DETACH_BYTES:' + data.hex(), flush=True)
elif 'context' in args:
    print(json.dumps({'Name':'external','DockerEndpoint':'unix:///external/docker.sock','Current':True}))
elif 'get' in args:
    print(json.dumps({'items':[{'metadata':{'name':'fixture-pod','namespace':'test','uid':'uid1'}}]}))
else:
    print('CLI_PASSTHROUGH', flush=True)
'''
    for name in ('docker', 'kubectl'):
        (tools / name).write_text(fixture)
        (tools / name).chmod(0o755)
    plugin = tools / 'kubectl-hamnfixture'
    plugin.write_text('#!/bin/sh\nexit 0\n'); plugin.chmod(0o755)
    config = root / 'kubeconfig'
    config.write_text(json.dumps({'apiVersion':'v1','kind':'Config','current-context':'dev',
        'contexts':[{'name':'dev','context':{'cluster':'dev','namespace':'test'}}],
        'clusters':[{'name':'dev','cluster':{'server':'http://127.0.0.1:1'}}]}))
    env = dict(os.environ, HOME=directory, PATH=f'{tools}:/usr/bin:/bin', TERM='xterm-256color',
               KUBECONFIG=str(config), CLI_RECORD=str(root / 'calls'))
    def run(saved=False, terminate_cli=False):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 140, 0, 0))
        before = termios.tcgetattr(slave)
        child = subprocess.Popen([binary], env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        output = bytearray()
        screen = Screen(32, 140)
        def until(marker):
            deadline = time.monotonic() + 15
            while marker.decode() not in screen.text():
                left = deadline - time.monotonic()
                assert left > 0 and select.select([master], [], [], left)[0], (marker, bytes(output[-3000:]))
                # Fragment redraws deliberately: a text marker alone must not
                # accept old rows remaining in a partially drawn settings view.
                data = os.read(master, 64)
                output.extend(data); screen.feed(data)
        def send(data, marker):
            output.clear(); os.write(master, data); until(marker)
        try:
            if saved:
                until(b'fixture-pod')
                assert b'Choose your default' not in output
                assert b'Hamn profile' not in output and b'VM settings' not in output
                if terminate_cli:
                    send(b':docker exec -it fixture-container sh\r', b'INPUT_READY')
                    if terminate_cli == 'exited':
                        send(b'xy', b'DETACH_BYTES:7879'); until(b'Exit code 0')
                    os.kill(child.pid, signal.SIGTERM)
            else:
                until(b'Choose your default workspace')
                assert not (root / '.hamn').exists()
                send(b'1\r', b'fixture-container')
                prefs = root / '.hamn/tui.json'
                assert json.loads(prefs.read_text()) == {'version':1,'defaultWorkspace':'containers'}
                assert prefs.stat().st_mode & 0o777 == 0o600
                send(b'e', b'external')
                send(b'\r', b'Docker context external')
                until(b'fixture-container')
                os.write(master, b'v')
                send(b':docker ps -q\r', b'RAW_OUTPUT')
                until(b'Exit code 7')
                send(b'\r', b'fixture-container')
                send(b':exec -it fixture-container sh\r', b'INPUT_READY')
                send(b'\x10\x11', b'DETACH_BYTES:1011')
                until(b'Exit code 0')
                send(b'\r', b'fixture-container')
                send(b'\t', b'fixture-pod')
                send(b':hamnfixture --custom-option value\r', b'CLI_PASSTHROUGH')
                until(b'Exit code 0')
                send(b'\r', b'fixture-pod')
                send(b',', b'Choose the workspace')
                send(b'2\r', b'fixture-pod')
                assert json.loads(prefs.read_text())['defaultWorkspace'] == 'kubernetes'
            if not terminate_cli: os.write(master, b'q')
            assert wait_for_exit(child, master, timeout=5) == 0
            after = termios.tcgetattr(slave)
            for modes in (before, after): modes[3] &= ~getattr(termios, 'PENDIN', 0)
            assert before == after
        finally:
            if child.poll() is None: os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=5)
            os.close(master); os.close(slave)
    run()
    run(saved=True)
    calls = [json.loads(line) for line in (root / 'calls').read_text().splitlines()]
    assert sum('-q' in args for _, args in calls) == 1, calls
    assert sum('exec' in args for _, args in calls) == 1, calls
    assert [args for _, args in calls if 'hamnfixture' in args] == [['hamnfixture', '--custom-option', 'value']], calls
    assert all('--format' not in args for _, args in calls if '-q' in args or 'exec' in args)
    assert sorted(p.name for p in (root / '.hamn').iterdir()) == ['tui.json'], 'TUI entry created VM state'
    run(saved=True, terminate_cli=True)
    run(saved=True, terminate_cli='exited')
print('PASS: workspace persistence, isolated scopes, native output, PTY input/detach and exact-once CLI dispatch')
