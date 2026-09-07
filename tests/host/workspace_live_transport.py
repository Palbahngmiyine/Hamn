"""Opt-in transport failure in the integration-owned VM; never start a VM here."""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess

from workspace_live_cancel_boundaries import BoundaryGate, HELPERS
from workspace_live_cancellation import wait_record


class BsdInfo(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint32) for name in (
        'flags', 'status', 'xstatus', 'pid', 'ppid', 'uid', 'gid', 'ruid',
        'rgid', 'svuid', 'svgid', 'reserved')] + [
        ('comm', ctypes.c_char * 16), ('name', ctypes.c_char * 32)] + [
        (name, ctypes.c_uint32) for name in ('nfiles', 'pgid', 'jobc', 'tdev', 'tpgid', 'nice')
    ] + [('startSec', ctypes.c_uint64), ('startUsec', ctypes.c_uint64)]


class Processes:
    def __init__(self):
        self.lib = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
        self.libc = ctypes.CDLL(None, use_errno=True)

    def identity(self, pid):
        info, usage, path = BsdInfo(), ctypes.create_string_buffer(512), ctypes.create_string_buffer(4096)
        assert self.lib.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) == ctypes.sizeof(info)
        assert info.pid == pid and info.uid == os.getuid()
        assert self.lib.proc_pid_rusage(pid, 0, usage) == 0
        assert self.lib.proc_pidpath(pid, path, len(path)) > 0
        return dict(pid=pid, ppid=info.ppid, startSec=info.startSec, startUsec=info.startUsec,
                    executableUuid=usage.raw[:16].hex(), path=os.fsdecode(path.value))

    def children(self, pid):
        buffer = (ctypes.c_int * 256)()
        count = self.lib.proc_listchildpids(pid, buffer, ctypes.sizeof(buffer))
        assert 0 <= count < ctypes.sizeof(buffer), 'child inventory failed or truncated'
        return [value for value in buffer if value > 0]

    def argv(self, pid):
        mib, buffer = (ctypes.c_int * 3)(1, 49, pid), ctypes.create_string_buffer(1024 * 1024)
        size = ctypes.c_size_t(len(buffer))
        assert self.libc.sysctl(mib, 3, buffer, ctypes.byref(size), None, 0) == 0
        raw = buffer.raw[:size.value]
        argc = int.from_bytes(raw[:4], 'little')
        assert 0 < argc < 4096
        offset = raw.index(b'\0', 4) + 1  # executable path precedes argv, then NUL padding
        while raw[offset] == 0:
            offset += 1
        args = raw[offset:].split(b'\0')[:argc]
        assert len(args) == argc
        return [os.fsdecode(arg) for arg in args]


def owned_ssh(processes, frontend, record, binary, profile):
    worker = processes.identity(record['pid'])
    assert worker['ppid'] == frontend and Path(worker['path']).resolve() == binary.resolve()
    assert all(worker[key] == record[key] for key in ('pid', 'startSec', 'startUsec', 'executableUuid'))
    candidates, pending = [], [(worker, [worker])]
    while pending:
        parent, chain = pending.pop()
        assert len(chain) <= 8, 'unexpected process ancestry'
        for pid in processes.children(parent['pid']):
            child = processes.identity(pid)
            assert child['ppid'] == parent['pid']
            if child['path'] == '/usr/bin/ssh':
                args = processes.argv(pid)
                if (str(profile / 'id_ed25519') in args and
                        'ControlPath=' + str(profile / 'ssh.sock') in args and
                        args[-1].find(HELPERS + '/guest-deployment-transaction') >= 0):
                    remote = shlex.split(args[-1])
                    if len(remote) >= 3 and remote[-2] == 'begin' and '/run/hamn-deployment.lock' in remote:
                        candidates.append((chain + [child], args))
            else:
                pending.append((child, chain + [child]))
    assert len(candidates) == 1, 'must identify exactly one owned deployment SSH child'
    chain, args = candidates[0]
    for identity in chain:
        assert processes.identity(identity['pid']) == identity, 'process identity changed'
    assert processes.argv(chain[-1]['pid']) == args, 'SSH arguments changed'
    return chain[-1]


def transport_failure(root, runtime, snapshot):
    owner = json.loads((root / 'ownership.json').read_bytes())
    assert owner['profile'] == 'verify'
    assert Path(owner['home']).resolve() == runtime.home.resolve() == (root / 'home').resolve()
    assert Path(owner['workspace']).resolve() == Path(__file__).resolve().parents[2]
    assert runtime.binary.resolve() == (root / 'hamn-under-test').resolve()
    profile = runtime.home / '.hamn/verify'
    path = profile / 'operation.json'
    runtime.call('vm', 'start', profile='verify', yes=True)
    original_pid, before = (profile / 'vmrun.pid').read_bytes(), snapshot(runtime)
    hash_command = f'sha256sum {HELPERS}/verify-image-contract {HELPERS}/guest-deployment-transaction {HELPERS}/configure-docker'
    hashes = runtime.ssh(hash_command, profile='verify')
    no_backups = 'test -z "$(ls -A /var/lib/hamn/deployment-transactions)"'
    runtime.ssh(no_backups, profile='verify')
    previous = json.loads(path.read_bytes())['operationId']
    gate, child = BoundaryGate(runtime, 'begin', True), None
    try:
        (profile / 'guest-deployment.version').unlink(missing_ok=True)
        child = subprocess.Popen([runtime.binary, '--headless', 'vm', 'start', '--profile', 'verify', '--yes'],
            env=runtime.environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        active = wait_record(path, lambda value: value.get('operationId') != previous, timeout=180)
        gate.wait()
        current = json.loads(path.read_bytes())
        assert current['operationId'] == active['operationId'] and current['status'] == 'running'
        assert child.poll() is None
        target = owned_ssh(Processes(), child.pid, active, runtime.binary, profile)
        os.kill(target['pid'], signal.SIGKILL)  # only the verified SSH child, never its process group
        stdout, stderr = child.communicate(timeout=240)  # original dispatch remains gated
        record = json.loads(path.read_bytes())
        assert child.returncode != 0 and record['status'] != 'cancelled', (record, stdout, stderr)
        assert b'fencing-after-failure' in stderr or record['phase'] == 'fencing-after-failure', record
        gate.release()
        runtime.ssh(f'python3 {gate.directory}/wait.py done; test "$(cat {gate.directory}/done)" = 130', profile='verify')
        runtime.ssh(no_backups, profile='verify')
        assert (profile / 'vmrun.pid').read_bytes() == original_pid
        assert snapshot(runtime) == before and runtime.ssh(hash_command, profile='verify') == hashes
        ready = runtime.call('vm', 'start', profile='verify', yes=True)
        assert ready['dockerStatus'] == 'ready'
        ping = subprocess.run(['/usr/bin/curl', '--silent', '--show-error', '--fail', '--unix-socket',
            str(profile / 'docker.sock'), 'http://localhost/_ping'], env=runtime.environment, capture_output=True, check=True)
        assert ping.stdout == b'OK' and (profile / 'vmrun.pid').read_bytes() == original_pid
        assert snapshot(runtime) == before
        (root / 'transport-failure-results.json').write_text(json.dumps(dict(
            ssh=target, operation=record, lateDispatchExit=130, dockerPing='OK',
            before=before, after=snapshot(runtime), helpersSha256=hashlib.sha256(hashes.encode()).hexdigest()), indent=2))
        print('PASS: owned SSH transport failure fences late begin, preserves Docker data and recovers /_ping', flush=True)
    finally:
        gate.release()
        if child and child.poll() is None:
            child.send_signal(signal.SIGINT)
            child.communicate(timeout=240)
        gate.close()


def test_ownership_guards():
    from copy import deepcopy
    from unittest.mock import Mock
    binary, profile = Path('/tmp/owned/hamn'), Path('/tmp/owned/home/.hamn/verify')
    worker = dict(pid=20, ppid=10, startSec=100, startUsec=2, executableUuid='ab' * 16, path=str(binary))
    supervisor = dict(worker, pid=30, ppid=20)
    ssh = dict(worker, pid=40, ppid=30, path='/usr/bin/ssh')
    graph = {20: worker, 30: supervisor, 40: ssh}
    args = ['ssh', '-i', str(profile / 'id_ed25519'), '-o', 'ControlPath=' + str(profile / 'ssh.sock'),
            shlex.join(['sudo', 'flock', '/run/hamn-deployment.lock', HELPERS + '/guest-deployment-transaction', 'begin', 'a' * 32])]
    for fault in (None, 'ppid', 'startSec', 'startUsec', 'executableUuid', 'key', 'action', 'changed', 'ambiguous'):
        values, actual_args = deepcopy(graph), list(args)
        if fault in worker:
            values[20][fault] = 'different' if fault == 'executableUuid' else 999
        if fault == 'key': actual_args[2] = '/tmp/unowned/id_ed25519'
        if fault == 'action': actual_args[-1] = actual_args[-1].replace('begin', 'commit')
        if fault == 'ambiguous': values[41] = dict(ssh, pid=41)
        process = Mock()
        process.identity.side_effect = lambda pid: deepcopy(values[pid])
        process.children.side_effect = lambda pid: [value['pid'] for value in values.values() if value['ppid'] == pid]
        process.argv.return_value = actual_args
        if fault == 'changed': process.argv.side_effect = [actual_args, args + ['changed']]
        try:
            result = owned_ssh(process, 10, worker, binary, profile)
        except AssertionError:
            assert fault is not None, 'valid owned process rejected'
        else:
            assert fault is None and result == ssh, 'unsafe process accepted: ' + str(fault)
    print('PASS: SSH ownership/ancestry/argument guards; physical transport test is opt-in')


if __name__ == '__main__':
    test_ownership_guards()
