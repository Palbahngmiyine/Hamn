"""Isolated VM/Docker and terminal helpers for the physical release harness."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import struct
import subprocess
import termios
import time


def run(command, env=None, timeout=660, data=None):
    result = subprocess.run([str(word) for word in command], env=env, input=data,
                            capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f'{command[0]} failed ({result.returncode}): {result.stderr[-4096:]} {result.stdout[-4096:]}')
    return result.stdout


class Runtime:
    def __init__(self, binary, home, docker):
        self.binary, self.home, self.docker = Path(binary), Path(home), docker
        self.environment = {'HOME': str(home), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'LC_ALL': 'C'}

    def call(self, *words, profile='default', **arguments):
        command = [self.binary, '--headless', *words, '--profile', profile]
        for key, value in arguments.items():
            command.append('--' + key.replace('_', '-'))
            if value is not True:
                command.append(str(value))
        output = run(command, self.environment)
        if words[-1:] == ('logs',) or (len(words) > 2 and words[2] == 'logs'):
            records = [json.loads(line) for line in output.splitlines()]
            if not records or any(record.get('ok') is not True for record in records):
                raise RuntimeError('invalid log stream result')
            result = records[-1]
        else:
            result = json.loads(output)
        if result.get('schemaVersion') != 1 or result.get('ok') is not True:
            raise RuntimeError(f'invalid headless result: {result}')
        return result['data']

    def engine(self, *words, profile='default'):
        socket = self.home / '.hamn' / profile / 'docker.sock'
        return run([self.docker, '--host', 'unix://' + str(socket), *words], self.environment)

    def ssh(self, script, profile='default'):
        status = self.call('vm', 'status', profile=profile)
        return run(['/usr/bin/ssh', '-F', 'none', '-i', self.home / '.hamn' / profile / 'id_ed25519',
                    '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'UserKnownHostsFile=/dev/null',
                    '-o', 'StrictHostKeyChecking=no', '-o', 'ConnectTimeout=10',
                    'hamn@' + status['ip'], shlex.join(['sudo', 'bash', '-euc', script])], self.environment)

    def snapshot(self, profile):
        result = {}
        for group, key in [('containers', 'Id'), ('images', 'Id'), ('volumes', 'Name'), ('networks', 'Id')]:
            rows = self.call('docker', group, 'list', profile=profile)
            result[group] = sorted(row[key] for row in rows)
        result['volumeSha256'] = self.engine('run', '--rm', '--pull=never', '--network=none',
            '--mount', 'type=volume,src=hamn-retirement-data,dst=/data,readonly',
            'busybox:1.37', 'sha256sum', '/data/sentinel', profile=profile).split()[0]
        return result

    def terminal(self, retiring=None):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
        before = termios.tcgetattr(slave)
        child = subprocess.Popen([self.binary], stdin=slave, stdout=slave, stderr=slave,
            env=dict(self.environment, TERM='xterm-256color'), start_new_session=True)
        output = bytearray()
        try:
            deadline = time.monotonic() + (630 if retiring else 15)
            while b'Hamn' not in output or (retiring and self.call('vm', 'status', profile=retiring)['migration'] != 'current'):
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not select.select([master], [], [], remaining)[0]:
                    raise RuntimeError('TUI did not render before the deadline')
                output.extend(os.read(master, 65536))
            os.write(master, b'q')
            if child.wait(timeout=10) or termios.tcgetattr(slave) != before:
                raise RuntimeError('TUI did not restore the terminal')
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=10)
            os.close(master)
            os.close(slave)

    def verify_retired(self, profile):
        script = '''
test "$(readlink /etc/systemd/system/k3s.service)" = /dev/null
test ! -e /usr/local/bin/k3s
test ! -e /var/lib/rancher/k3s
test ! -e /etc/rancher/k3s
test ! -e /etc/hamn/k3s-compatibility.json
test "$(ctr --namespace k8s.io containers list -q | wc -l)" -eq 0
cat /var/lib/hamn/k3s-retirement-v1.json
'''
        journal = json.loads(self.ssh(script, profile=profile))
        if journal != {'version': 1, 'stage': 'complete'}:
            raise RuntimeError('retirement journal is incomplete')

    def stop(self, profiles):
        errors = []
        for profile in profiles:
            try:
                self.call('vm', 'stop', profile=profile, yes=True)
                if self.call('vm', 'status', profile=profile)['state'] != 'stopped':
                    errors.append(profile)
            except Exception as error:
                errors.append(str(error))
        if errors:
            raise RuntimeError('physical test cleanup failed; workspace retained: ' + '; '.join(errors))
