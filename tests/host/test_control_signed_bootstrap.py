#!/usr/bin/env python3
"""Exercise the public Rust start/retry path with an owned updater fixture.

This tests the handoff protocol, not signature verification or a physical VM.
The fixture populates only its private cache and terminates the retry before VM work.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

BINARY = Path(os.environ.get('HAMN', 'build/hamn')).resolve()
UPDATER = r'''
import json, os, sys
from pathlib import Path
root = Path(os.environ['HOME'])
with (root / 'calls').open('a') as out:
    out.write(json.dumps(sys.argv[1:]) + '\n')
if sys.argv[1:] == ['--headless', 'system', 'update', '--yes']:
    cache = root / '.hamn/cache'
    cache.mkdir(parents=True, exist_ok=True)
    digest = '0123456789abcdef' * 4
    name = 'hamn-guest-' + digest + '.img'
    for file, text in [(name, 'fixture'), (name + '.verified', digest),
        ('guest-image.json', json.dumps({'schemaVersion': 1, 'file': name, 'sha256': digest}))]:
        path = cache / file
        path.write_text(text)
        path.chmod(0o600)
elif sys.argv[1] == '__core-worker':
    request = json.load(sys.stdin)
    assert request['words'] == ['vm', 'start'] and request['profile'] == 'bootstrap'
    print(json.dumps({'Ok': {'bootstrapRetry': True}}))
else:
    raise AssertionError(sys.argv)
'''


def main():
    for previous_unknown in (False, True):
        with tempfile.TemporaryDirectory(prefix='hamn-bootstrap-public-', dir='/tmp') as directory:
            root = Path(directory)
            updater = root / 'hamn'
            updater.write_text(f'#!{sys.executable}\n' + UPDATER)
            updater.chmod(0o700)
            if previous_unknown:
                profile = root / '.hamn/bootstrap'
                profile.mkdir(parents=True)
                record = profile / 'operation.json'
                record.write_text(json.dumps({'schemaVersion': 1, 'operationId': 'a' * 32,
                    'status': 'outcomeUnknown', 'pid': 1, 'startSec': 0, 'startUsec': 0,
                    'executableUuid': '0' * 32}))
                record.chmod(0o600)
            # argv[0] identifies the installation that the real worker invokes
            # for update, then the public Rust frontend uses for its one retry.
            env = dict(os.environ, HOME=str(root))
            result = subprocess.run([str(updater), '--headless', 'vm', 'start',
                '--profile', 'bootstrap', '--yes'], executable=str(BINARY), env=env,
                capture_output=True, text=True, timeout=20)
            assert result.returncode == 0, (result.stdout, result.stderr)
            envelope = json.loads(result.stdout.splitlines()[-1])
            assert envelope['data']['bootstrapRetry'] is True, envelope
            calls = [json.loads(line) for line in (root / 'calls').read_text().splitlines()]
            assert calls == [['--headless', 'system', 'update', '--yes'],
                ['__core-worker', str(updater)]], calls
            record = json.loads((root / '.hamn/bootstrap/operation.json').read_text())
            assert record['status'] == 'restartRequired' and record['exitCode'] == 3, record
            assert record['phase'] == 'signed-image-ready' and record['error'] == '', record
            assert bool(record.get('recoveryRequired')) == previous_unknown, record
            status = subprocess.run([str(BINARY), '--headless', 'vm', 'status', '--profile',
                'bootstrap'], env=env, capture_output=True, text=True, timeout=10)
            assert status.returncode == 0, status.stderr
            data = json.loads(status.stdout)['data']
            assert data['dockerStatus'] == ('recoveryRequired' if previous_unknown else 'unavailable'), data
            assert not list((root / '.hamn/bootstrap').glob('*.sock'))
            assert not (root / '.hamn/bootstrap/vmrun.pid').exists()
            assert not list((root / '.hamn/bootstrap').glob('*.img'))
    print('PASS: public start retries the installed binary exactly once after image preparation; prior uncertainty retained')


if __name__ == '__main__':
    main()
