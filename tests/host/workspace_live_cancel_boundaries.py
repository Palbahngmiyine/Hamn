"""Opt-in cancellation barriers inside the workspace-owned physical VM only.

The retirement case resumes an already-complete retirement journal with an
explicit legacy-profile fixture. It tests retirement_run cancellation/cleanup,
not destruction of a live K3s installation. No signed helper is replaced.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import uuid
from workspace_live_cancellation import wait_record

WRAPPER = '/usr/local/bin/flock'
HELPERS = '/usr/local/libexec/hamn'


def wrapper_source(directory, action, before_lock=False):
    return f'''#!/usr/bin/python3
import os, pathlib, sys
root = pathlib.Path({directory!r})
args = sys.argv[1:]
action = {action!r}
before_lock = {before_lock!r}
lock = '/run/hamn-retirement.lock' if action == 'retirement' else '/run/hamn-deployment.lock'
match = lock in args and ((action == 'retirement' and 'recover-only' not in args and 'python3' in args)
    or (len(args) > 2 and args[-2] == action and args[-3].endswith('/guest-deployment-transaction')))
if match:
    try:
        (root / 'claimed').mkdir()
    except FileExistsError:
        match = False
if match:
    if before_lock:
        os.execv('/usr/bin/python3', ['python3', str(root / 'gate.py'), '/usr/bin/flock'] + args)
    index = args.index(lock) + 1
    args = args[:index] + ['/usr/bin/python3', str(root / 'gate.py')] + args[index:]
os.execv('/usr/bin/flock', ['flock'] + args)
'''


WAIT = '''import ctypes, os, pathlib, select, sys
root = pathlib.Path(__file__).parent
libc = ctypes.CDLL(None, use_errno=True)
fd = libc.inotify_init1(os.O_CLOEXEC)
assert fd >= 0
assert libc.inotify_add_watch(fd, os.fsencode(root), 0x100 | 0x80 | 0x8) >= 0
try:
    while not (root / sys.argv[1]).exists():
        assert select.select([fd], [], [], 180)[0], 'guest gate deadline exceeded'
        os.read(fd, 65536)
finally:
    os.close(fd)
print('LOCK_READY' if sys.argv[1] == 'ready' else 'RELEASED', flush=True)
'''

GATE = '''import os, pathlib, signal, subprocess, sys
root = pathlib.Path(__file__).parent
for number in (signal.SIGHUP, signal.SIGTERM, signal.SIGINT):
    signal.signal(number, signal.SIG_IGN)
(root / 'ready').write_text('LOCK_READY\\n')
subprocess.run(['/usr/bin/python3', str(root / 'wait.py'), 'released'], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
result = subprocess.run(sys.argv[1:])
(root / 'done').write_text(str(result.returncode))
sys.exit(result.returncode)
'''



class BoundaryGate:
    def __init__(self, runtime, action, before_lock=False):
        self.runtime = runtime
        self.directory = '/var/lib/hamn-workspace-cancel-' + uuid.uuid4().hex
        self.source = wrapper_source(self.directory, action, before_lock)
        self.released = False
        self.observer = None
        setup = f'''test ! -e {WRAPPER}; test ! -L {WRAPPER}
test "$(command -v flock)" = /usr/bin/flock
mkdir -m 700 {self.directory}
printf %s {shlex.quote(WAIT)} > {self.directory}/wait.py
printf %s {shlex.quote(GATE)} > {self.directory}/gate.py
printf %s {shlex.quote(self.source)} > {WRAPPER}
chmod 755 {WRAPPER}
test "$(command -v flock)" = {WRAPPER}
'''
        runtime.ssh(setup, profile='verify')

    def wait(self):
        status = self.runtime.call('vm', 'status', profile='verify')
        args = ['/usr/bin/ssh', '-F', 'none', '-i', self.runtime.home / '.hamn/verify/id_ed25519',
                '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'ConnectTimeout=10',
                '-o', 'StrictHostKeyChecking=no', '-o', 'UserKnownHostsFile=/dev/null',
                'hamn@' + status['ip'], shlex.join(['sudo', '/usr/bin/python3', self.directory + '/wait.py', 'ready'])]
        self.observer = subprocess.Popen(args, env=self.runtime.environment,
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        stdout, stderr = self.observer.communicate(timeout=180)
        assert self.observer.returncode == 0 and stdout == b'LOCK_READY\n', (stdout, stderr)

    def release(self):
        if self.released:
            return
        self.runtime.ssh('touch ' + self.directory + '/released', profile='verify')
        self.released = True

    def close(self):
        self.release()
        if self.observer and self.observer.poll() is None:
            self.observer.terminate()
            self.observer.communicate(timeout=15)
        expected = hashlib.sha256(self.source.encode()).hexdigest()
        # Only remove our exact wrapper; never replace a concurrent user's file.
        self.runtime.ssh(f'''test "$(sha256sum {WRAPPER} | cut -d' ' -f1)" = {expected}
/usr/bin/flock --wait 120 /run/hamn-deployment.lock true
/usr/bin/flock --wait 600 /run/hamn-retirement.lock true
rm {WRAPPER}
rm -r {self.directory}
''', profile='verify')


def cancellation_boundaries(root, runtime, snapshot):
    owner = json.loads((root / 'ownership.json').read_bytes())
    assert owner['profile'] == 'verify'
    assert Path(owner['home']).resolve() == runtime.home.resolve() == (root / 'home').resolve()
    assert Path(owner['workspace']).resolve() == Path(__file__).resolve().parents[2]
    path = runtime.home / '.hamn/verify/operation.json'
    config = path.parent / 'config.yaml'
    before = snapshot(runtime)
    hashes = runtime.ssh(f'sha256sum {HELPERS}/verify-image-contract {HELPERS}/guest-deployment-transaction {HELPERS}/configure-docker', profile='verify')
    evidence = []
    cases = [('refresh', 'begin'), ('refresh', 'commit'), ('reconcile', 'begin'),
             ('reconcile', 'commit'), ('retirement', 'retirement')]
    for mode, action, queued in [(m, a, False) for m, a in cases] + [
            (m, a, True) for m, a in cases if m != 'reconcile']:
        runtime.call('vm', 'start', profile='verify', yes=True)
        original_pid = (path.parent / 'vmrun.pid').read_bytes()
        original_config = config.read_bytes()
        gate = BoundaryGate(runtime, action, queued)
        child = None
        try:
            if mode == 'refresh':
                (path.parent / 'guest-deployment.version').unlink(missing_ok=True)
            elif mode == 'reconcile':
                runtime.call('vm', 'stop', profile='verify', yes=True)
            else:
                # A complete journal avoids deleting any real K3s workloads.
                runtime.ssh("python3 -c \"import json; assert json.load(open('/var/lib/hamn/k3s-retirement-v1.json')) == {'version': 1, 'stage': 'complete'}\"", profile='verify')
                assert b'kubernetes:' not in original_config
                config.write_bytes(original_config + b'\nkubernetes:\n  enabled: false\n  version: v1.35.1+k3s1\n')
            previous = json.loads(path.read_bytes()).get('operationId')
            operation = 'migrate' if mode == 'retirement' else 'start'
            child = subprocess.Popen([runtime.binary, '--headless', 'vm', operation,
                '--profile', 'verify', '--yes'], env=runtime.environment,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            active = wait_record(path, lambda value: value.get('operationId') != previous and
                (mode != 'reconcile' or value.get('phase') == 'guest-deployment-reused'), timeout=180)
            gate.wait()
            child.send_signal(signal.SIGINT)
            if queued:
                # The original dispatch has not acquired the lock. Cleanup may
                # finish first, but its retained token must reject that dispatch.
                stdout, stderr = child.communicate(timeout=240)
                gate.release()
            else:
                wait_record(path, lambda value: value.get('phase') == 'fencing-after-cancel', timeout=30)
                lock = '/run/hamn-retirement.lock' if mode == 'retirement' else '/run/hamn-deployment.lock'
                runtime.ssh(f'! /usr/bin/flock -n {lock} true', profile='verify')
                assert child.poll() is None, 'frontend exited while original writer held its lock'
                gate.release()
                stdout, stderr = child.communicate(timeout=240)
            record = json.loads(path.read_bytes())
            assert child.returncode != 0 and record['status'] == 'cancelled', (record, stdout, stderr)
            if mode != 'reconcile':
                runtime.ssh(f'python3 {gate.directory}/wait.py done; test \"$(cat {gate.directory}/done)\" = 130', profile='verify')
            if mode == 'reconcile':
                assert runtime.call('vm', 'status', profile='verify')['state'] == 'stopped'
                runtime.call('vm', 'start', profile='verify', yes=True)
            else:
                assert runtime.call('vm', 'status', profile='verify')['state'] == 'running'
                assert (path.parent / 'vmrun.pid').read_bytes() == original_pid
            runtime.ssh('test -z "$(ls -A /var/lib/hamn/deployment-transactions)"', profile='verify')
            assert snapshot(runtime) == before
            assert runtime.ssh(f'sha256sum {HELPERS}/verify-image-contract {HELPERS}/guest-deployment-transaction {HELPERS}/configure-docker', profile='verify') == hashes
            evidence.append({'mode': mode, 'action': action, 'queuedBeforeLock': queued, 'operation': record})
            print(f'PASS: actual {mode}/{action} queued={queued} cancellation waits for remote writer and preserves Docker data', flush=True)
        finally:
            # Reach a running owned VM before SSH cleanup if cancellation stopped it.
            status = runtime.call('vm', 'status', profile='verify')
            if status['state'] == 'stopped':
                runtime.call('vm', 'start', profile='verify', yes=True)
            gate.release()
            if child and child.poll() is None:
                child.send_signal(signal.SIGINT)
                child.communicate(timeout=240)
            gate.close()
            if mode == 'retirement':
                config.write_bytes(original_config)
    (root / 'cancel-boundary-results.json').write_text(json.dumps(evidence, indent=2))


if __name__ == '__main__':
    compile(wrapper_source('/var/lib/hamn-workspace-cancel-test', 'begin'), '<wrapper>', 'exec')
    compile(GATE, '<gate>', 'exec')
    compile(WAIT, '<wait>', 'exec')
    import tempfile
    from unittest.mock import patch
    class Dispatched(Exception):
        pass
    for action in ('begin', 'commit', 'retirement'):
        for queued in (False, True):
            with tempfile.TemporaryDirectory(prefix='hamn-flock-fixture-') as directory:
                lock = '/run/hamn-retirement.lock' if action == 'retirement' else '/run/hamn-deployment.lock'
                argv = ['flock', '--wait', '120', lock, 'python3' if action == 'retirement' else 'bash', HELPERS + '/guest-deployment-transaction', action, 'a' * 32]
                source = wrapper_source(directory, action, queued)
                with patch('sys.argv', argv), patch('os.execv', side_effect=Dispatched) as execute:
                    for first in (True, False):
                        try:
                            exec(compile(source, '<wrapper>', 'exec'), {})
                        except Dispatched:
                            pass
                        args = execute.call_args.args
                        assert args[0] == ('/usr/bin/python3' if queued and first else '/usr/bin/flock')
                        assert (directory + '/gate.py' in args[1]) == first
                        if not first:
                            assert args[1] == argv
    print('PASS: fixture syntax and holder/queued one-shot dispatch; physical execution is opt-in')
