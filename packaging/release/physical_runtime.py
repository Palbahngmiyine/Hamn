"""Isolated VM/Docker helpers kept only for the workspace_live Python tests.

The physical release harness is tools/hamn-dev/src/release (runtime.rs holds
the Rust equivalent of this module). Delete this file when the live tests
that import it (tests/host/test_workspace_live.py, workspace_live_*.py and
test_guest_image_live.py) are ported.
"""
import json
from pathlib import Path
import shlex
import subprocess


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
