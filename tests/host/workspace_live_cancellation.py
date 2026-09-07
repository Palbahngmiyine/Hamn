"""Deterministic guest lock barriers for real navigation/cancel/forced-exit checks."""
import json
import os
import select
import shlex
import signal
import subprocess
import time
from workspace_live_terminal import Terminal


def wait_record(path, predicate, timeout=30, terminal=None):
    directory = os.open(path.parent, os.O_RDONLY)
    queue = select.kqueue()
    event = select.kevent(directory, filter=select.KQ_FILTER_VNODE,
        flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR, fflags=select.KQ_NOTE_WRITE)
    queue.control([event], 0, 0)
    try:
        deadline = time.monotonic() + timeout
        while True:
            value = json.loads(path.read_text()) if path.exists() else {}
            if predicate(value): return value
            left = deadline - time.monotonic()
            assert left > 0, value
            ready = select.select([queue] + ([terminal.master] if terminal else []), [], [], left)[0]
            assert ready, value
            if queue in ready: queue.control(None, 1, 0)
            if terminal and terminal.master in ready:
                data = os.read(terminal.master, 65536)
                terminal.record.write(data); terminal.screen.feed(data)
    finally:
        queue.close(); os.close(directory)


class Gate:
    def __init__(self, runtime):
        self.runtime = runtime
        status = runtime.call('vm', 'status', profile='verify')
        runtime.ssh('mkdir -m 700 /run/hamn-workspace-gate; mkfifo /run/hamn-workspace-gate/release', profile='verify')
        command = ['sudo','flock','/run/hamn-retirement.lock','bash','-c',
            'echo LOCK_READY; read token < /run/hamn-workspace-gate/release; test "$token" = release']
        self.child = subprocess.Popen(['/usr/bin/ssh','-F','none','-i',runtime.home / '.hamn/verify/id_ed25519',
            '-o','BatchMode=yes','-o','IdentitiesOnly=yes','-o','StrictHostKeyChecking=no',
            '-o','UserKnownHostsFile=/dev/null','hamn@' + status['ip'],shlex.join(command)],
            env=runtime.environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert select.select([self.child.stdout], [], [], 15)[0]
        assert self.child.stdout.readline() == b'LOCK_READY\n'
        self.released = False

    def release(self):
        if self.released: return
        self.runtime.ssh('printf "release\\n" > /run/hamn-workspace-gate/release', profile='verify')
        assert self.child.wait(timeout=15) == 0
        self.runtime.ssh('rm /run/hamn-workspace-gate/release; rmdir /run/hamn-workspace-gate', profile='verify')
        self.released = True


def cancellation(root, runtime):
    path = runtime.home / '.hamn/verify/operation.json'
    vm_pid = (runtime.home / '.hamn/verify/vmrun.pid').read_text()
    gate = Gate(runtime)
    terminal = Terminal(runtime.binary, runtime.environment, root)
    results = []
    try:
        terminal.until('hamn-workspace-sentinel')
        terminal.send(b':vm start --profile verify\r', 'Confirm vm start')
        terminal.send(b'y!','recovering-deployment')
        active = wait_record(path, lambda value: value.get('phase') == 'recovering-deployment' and value.get('status') == 'running')
        terminal.send(b'\x1b\t', 'Kubernetes')
        assert json.loads(path.read_text())['operationId'] == active['operationId']
        assert json.loads(path.read_text())['status'] == 'running'
        terminal.send(b'q', 'Cancel the active operation and exit?')
        terminal.send(b'y')
        wait_record(path, lambda value: value.get('phase') == 'fencing-after-cancel', terminal=terminal)
        assert terminal.child.poll() is None, 'quit did not wait for remote cleanup'
        gate.release()
        completed = wait_record(path, lambda value: value.get('status') != 'running', timeout=60, terminal=terminal)
        assert completed['status'] == 'cancelled', completed
        results.append(completed)
    finally:
        gate.release()
        terminal.close()
    assert (runtime.home / '.hamn/verify/vmrun.pid').read_text() == vm_pid
    assert runtime.call('vm','status',profile='verify')['dockerStatus'] == 'ready'
    print('PASS: navigation preserves start; confirmed quit waits for remote cleanup; existing VM survives', flush=True)
    gate = Gate(runtime)
    child = subprocess.Popen([runtime.binary,'--headless','vm','start','--profile','verify','--yes'],
        env=runtime.environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        active = wait_record(path, lambda value: value.get('operationId') != results[-1]['operationId'] and value.get('phase') == 'recovering-deployment')
        # This PID is the freshly launched operation's exact recorded worker, not an unrelated VM.
        os.kill(active['pid'], signal.SIGKILL)
        stdout, stderr = child.communicate(timeout=15)
        assert child.returncode != 0
        result = json.loads(stdout)
        assert result['error']['code'] == 'outcomeUnknown', result
        status = runtime.call('vm','status',profile='verify')
        assert status['dockerStatus'] == 'recoveryRequired'
        gate.release()
        result = runtime.call('vm','start',profile='verify',yes=True)
        assert result['dockerStatus'] == 'ready'
        assert (runtime.home / '.hamn/verify/vmrun.pid').read_text() == vm_pid
        results.append(result)
    finally:
        gate.release()
        if child.poll() is None: child.terminate(); child.wait(timeout=15)
    (root / 'cancellation-results.json').write_text(json.dumps(results, indent=2))
    print('PASS: forced worker death retains outcomeUnknown; retry repairs without terminating the existing VM', flush=True)


def owned_start_cancellation(root, runtime):
    profile = 'cancel-owned'
    path = runtime.home / '.hamn' / profile / 'operation.json'
    path.parent.mkdir(mode=0o700, exist_ok=True)
    previous = json.loads(path.read_text()).get('operationId') if path.exists() else None
    child = subprocess.Popen([runtime.binary, '--headless', 'vm', 'start', '--profile', profile,
        '--cpu', '2', '--memory', '2', '--disk', '60', '--yes'], env=runtime.environment,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        wait_record(path, lambda value: value.get('operationId') != previous and value.get('startedVm') is True, timeout=180)
        child.send_signal(signal.SIGINT)
        stdout, stderr = child.communicate(timeout=180)
        status = runtime.call('vm', 'status', profile=profile)
        assert status['state'] == 'stopped', status
        assert status['lastOperation']['status'] == 'cancelled', status
        (root / 'owned-cancel-result.json').write_text(json.dumps(status, indent=2))
        print('PASS: cancelled start stops only its newly created VM', flush=True)
    finally:
        if child.poll() is None:
            child.send_signal(signal.SIGINT); child.communicate(timeout=180)
        runtime.stop([profile])
