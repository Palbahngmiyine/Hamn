#!/usr/bin/env python3
"""Exercise rejected start input with the built binary; never start a VM."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

binary = Path(os.environ.get('HAMN', 'build/hamn')).resolve()
with tempfile.TemporaryDirectory(prefix='hamn-start-preflight-') as temporary:
    root = Path(temporary)
    env = dict(os.environ, HOME=str(root))

    def call(*words, code=0):
        result = subprocess.run([binary, '--headless', *words], env=env,
                                text=True, capture_output=True, timeout=15)
        assert result.returncode == code, result
        return json.loads(result.stdout)

    call('vm', 'create', '--profile', 'fixture', '--disk', '20', '--yes')
    profile = root / '.hamn/fixture'
    before = (profile / 'config.yaml').read_bytes()
    result = call('vm', 'start', '--profile', 'fixture', '--disk', '10', '--yes', code=1)
    assert result['error']['code'] == 'operationFailed', result
    assert 'disk size cannot shrink' in result['error']['message'], result
    state = call('vm', 'status', '--profile', 'fixture')['data']
    assert state['state'] == 'stopped' and state['dockerStatus'] == 'unavailable', state
    assert state['lastOperation']['status'] == 'failed', state
    assert state['lastOperation']['startedVm'] is False, state
    assert state['diskGiB'] == 20 and (profile / 'config.yaml').read_bytes() == before
    assert not any((profile / file).exists() for file in ('vmrun.pid', 'vmrun.identity', 'docker.sock'))
    # A rejected command must not erase an unresolved earlier operation.
    record = profile / 'operation.json'
    value = json.loads(record.read_bytes())
    value['status'] = 'outcomeUnknown'
    record.write_text(json.dumps(value))
    result = call('vm', 'start', '--profile', 'fixture', '--disk', '10', '--yes', code=1)
    assert result['error']['code'] == 'outcomeUnknown', result
    state = call('vm', 'status', '--profile', 'fixture')['data']
    assert state['dockerStatus'] == 'recoveryRequired', state
    assert (profile / 'config.yaml').read_bytes() == before
print('PASS: rejected start preserves stopped VM/configuration and reports known failure')
