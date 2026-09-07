#!/usr/bin/env python3
"""Native CLI regressions using a real Hamn PTY and disposable, recorded CLI peers."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from terminal_screen import RatatuiScreen as Screen, wait_for_exit

BINARY = Path(os.environ.get('HAMN', 'build/hamn')).resolve()
FIXTURE = r'''
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
root = Path(os.environ['FIXTURE_ROOT'])
with (root / 'calls').open('a') as out:
    out.write(json.dumps([Path(sys.argv[0]).name, args]) + '\n')
if 'config' in args:
    if 'view' in args:
        print(json.dumps({'current-context': 'new-cluster', 'contexts': [
            {'name': 'new-cluster', 'context': {'namespace': 'test'}}]}))
    else:
        print('CONFIG_DONE', flush=True)
elif 'context' in args:
    if 'ls' in args:
        print(json.dumps({'Name': 'external', 'Current': True,
                          'DockerEndpoint': 'unix:///external/docker.sock'}))
    else:
        print('CONFIG_DONE', flush=True)
elif 'events' in args:
    print('EVENTS_DONE', flush=True)
elif 'ps' in args or 'get' in args:
    changed = 'new-cluster' in args or 'external' in args
    if changed and not (root / 'released').exists():
        with (root / 'notice').open('w') as out:
            out.write('query-blocked\n'); out.flush()
        with (root / 'gate').open() as gate:
            gate.read(1)
    name = 'new-target-row' if changed else 'old-target-row'
    if 'ps' in args:
        if '-n' in args or '-n5' in args: name = 'last-five-row'
        if '-s' in args: name = 'size-row'
        print(json.dumps({'ID': 'abc123', 'Names': name, 'State': 'running'}))
    else:
        print(json.dumps({'items': [{'metadata': {
            'name': name, 'namespace': 'test', 'uid': 'uid-original'}}]}))
else:
    print('ACTION_DONE', flush=True)
'''


class Harness:
    def __init__(self, workspace):
        self.directory = tempfile.TemporaryDirectory(prefix='hamn-native-regression-', dir='/tmp')
        self.root = Path(self.directory.name)
        (self.root / 'bin').mkdir()
        (self.root / '.hamn').mkdir(mode=0o700)
        prefs = self.root / '.hamn/tui.json'
        prefs.write_text(json.dumps({'version': 1, 'defaultWorkspace': workspace}))
        prefs.chmod(0o600)
        for name in ('docker', 'kubectl'):
            path = self.root / 'bin' / name
            path.write_text(f'#!{sys.executable}\n' + FIXTURE)
            path.chmod(0o755)
        config = self.root / 'kubeconfig'
        config.write_text(json.dumps({'apiVersion': 'v1', 'kind': 'Config',
            'current-context': 'old-cluster', 'contexts': [{'name': 'old-cluster',
            'context': {'cluster': 'fixture', 'namespace': 'test'}}],
            'clusters': [{'name': 'fixture', 'cluster': {'server': 'http://127.0.0.1:1'}}]}))
        for name in ('notice', 'gate'):
            os.mkfifo(self.root / name)
        self.notice = os.open(self.root / 'notice', os.O_RDWR | os.O_NONBLOCK)
        self.gate = os.open(self.root / 'gate', os.O_RDWR | os.O_NONBLOCK)
        env = dict(os.environ, HOME=str(self.root), FIXTURE_ROOT=str(self.root),
                   PATH=f'{self.root}/bin:/usr/bin:/bin', TERM='xterm-256color', KUBECONFIG=str(config))
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 160, 0, 0))
        self.child = subprocess.Popen([BINARY], stdin=self.slave, stdout=self.slave,
                                      stderr=self.slave, env=env, start_new_session=True)
        self.screen = Screen(32, 160)
        self.notices = b''

    def wait(self, predicate):
        deadline = time.monotonic() + 15
        while not predicate():
            remaining = deadline - time.monotonic()
            assert remaining > 0, self.screen.text()
            ready, _, _ = select.select([self.master, self.notice], [], [], remaining)
            assert ready, self.screen.text()
            for fd in ready:
                data = os.read(fd, 65536)
                assert data, 'PTY closed before the expected result'
                if fd == self.master:
                    self.screen.feed(data)
                else:
                    self.notices += data

    def until(self, text):
        self.wait(lambda: text in self.screen.text())

    def send(self, keys, text):
        os.write(self.master, keys)
        self.until(text)

    def calls(self):
        return [json.loads(line) for line in (self.root / 'calls').read_text().splitlines()]

    def close(self):
        if self.child.poll() is None:
            os.kill(self.child.pid, signal.SIGTERM)
            try:
                wait_for_exit(self.child, self.master, timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(self.child.pid, signal.SIGKILL)
                self.child.wait(timeout=5)
        for fd in (self.master, self.slave, self.notice, self.gate):
            os.close(fd)
        self.directory.cleanup()


def changed_target_invalidates_previous_rows(workspace, command):
    harness = Harness(workspace)
    try:
        harness.until('old-target-row')
        harness.send(b':' + command.encode() + b'\r', 'CONFIG_DONE')
        harness.until('Exit code 0')
        os.write(harness.master, b'\r')
        harness.wait(lambda: b'query-blocked' in harness.notices)
        # The FIFO keeps the new target unresolved. Input marker establishes that
        # the delete key was processed without opening an old-row confirmation.
        os.write(harness.master, b'd:REGRESSION_BARRIER')
        harness.wait(lambda: 'Confirm delete' in harness.screen.text() or
                     ':REGRESSION_BARRIER' in harness.screen.text())
        assert 'Confirm delete' not in harness.screen.text(), harness.screen.text()
        assert 'old-target-row' not in harness.screen.text(), harness.screen.text()
        assert not any('delete' in args or 'rm' in args for _, args in harness.calls())
        os.write(harness.master, b'\x1b')
        (harness.root / 'released').touch()
        os.write(harness.gate, b'1')
        harness.until('new-target-row')
    finally:
        harness.close()


def docker_list_options_do_not_become_action_connections(option, row):
    harness = Harness('containers')
    try:
        harness.until('old-target-row')
        harness.send(f':docker ps {option}\r'.encode(), row)
        harness.send(b'\r', 'ACTION_DONE')
        harness.until('Exit code 0')
        actions = [args for program, args in harness.calls() if program == 'docker' and 'inspect' in args]
        assert len(actions) == 1, actions
        assert actions[0] == ['--host', f'unix://{harness.root}/.hamn/default/docker.sock',
                              'container', 'inspect', 'abc123'], actions
    finally:
        harness.close()


def native_events_is_not_rewritten(prefix):
    harness = Harness('kubernetes')
    try:
        harness.until('old-target-row')
        harness.send(f':{prefix}events --for pod/example --watch\r'.encode(), 'EVENTS_DONE')
        harness.until('Exit code 0')
        calls = [args for program, args in harness.calls() if program == 'kubectl' and 'events' in args]
        assert calls == [['--context', 'old-cluster', '--namespace', 'test',
                          'events', '--for', 'pod/example', '--watch']], calls
    finally:
        harness.close()


def installed_plugins_own_alias_names(prefix, alias):
    harness = Harness('kubernetes')
    try:
        harness.until('old-target-row')
        plugin = harness.root / 'bin' / ('kubectl-' + alias)
        plugin.write_text(f'#!{sys.executable}\n' + '''import json, os, sys
from pathlib import Path
Path(os.environ['FIXTURE_ROOT'], 'plugin-call').write_text(json.dumps(sys.argv[1:]))
print('PLUGIN_RAN', flush=True)
''')
        plugin.chmod(0o755)
        kubectl = harness.root / 'bin/kubectl'
        original = kubectl.read_text()
        dispatch = f'''if args and args[0] == {alias!r}:
    plugin = root / 'bin' / {plugin.name!r}
    os.execv(str(plugin), [str(plugin)] + args[1:])
elif 'config' in args:'''
        kubectl.write_text(original.replace("if 'config' in args:", dispatch))
        harness.send(f':{prefix}{alias} review-space\r'.encode(), 'PLUGIN_RAN')
        harness.until('Exit code 0')
        assert json.loads((harness.root / 'plugin-call').read_text()) == ['review-space']
        calls = [args for _, args in harness.calls() if 'review-space' in args]
        assert calls == [[alias, 'review-space']], calls
        assert 'Plugin-defined target' in harness.screen.text()
    finally:
        harness.close()


if __name__ == '__main__':
    for workspace, command in [('kubernetes', 'kubectl config use-context new-cluster'),
                               ('containers', 'docker context use external')]:
        changed_target_invalidates_previous_rows(workspace, command)
    for option, row in [('-n 5', 'last-five-row'), ('-n5', 'last-five-row'), ('-s', 'size-row')]:
        docker_list_options_do_not_become_action_connections(option, row)
    for prefix in ('kubectl ', ''):
        native_events_is_not_rewritten(prefix)
        for alias in ('ns', 'pods'):
            installed_plugins_own_alias_names(prefix, alias)
    print('PASS: changed targets discard old rows; Docker flags, events and installed plugins preserve CLI semantics')
