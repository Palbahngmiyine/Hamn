#!/usr/bin/env python3
"""Exercise strict C profile parsing through the public headless contract."""
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile

binary = Path(os.environ.get('HAMN', 'build/hamn')).resolve()

with tempfile.TemporaryDirectory(prefix='hamn-yaml-') as directory:
    root = Path(directory)
    env = dict(os.environ, HOME=directory)
    def run(*args):
        result = subprocess.run([binary, '--headless', *args], env=env,
                                capture_output=True, text=True, timeout=15)
        value = json.loads(result.stdout)
        assert value['ok'] == (result.returncode == 0), value
        return value

    def write(name, text):
        path = root / '.hamn' / name / 'config.yaml'
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return path

    assert not run('vm', 'status')['ok']
    assert not (root / '.hamn').exists()
    assert not run('vm', 'create', '--profile', 'test')['ok']
    assert not (root / '.hamn').exists()
    assert run('vm', 'create', '--profile', 'test', '--cpu', '6', '--memory', '8', '--disk', '80', '--yes')['ok']
    config = root / '.hamn/test/config.yaml'
    assert stat.S_IMODE(config.stat().st_mode) == 0o600
    text = config.read_text()
    assert 'cpus: 6' in text and 'memoryMiB: 8192' in text and 'diskGiB: 80' in text
    assert 'kubernetes:' not in text
    value = run('vm', 'env', '--profile', 'test')
    assert value['data']['DOCKER_HOST'] == f'unix://{root}/.hamn/test/docker.sock'
    for arguments in [('--cpu', '0'), ('--memory', '0'), ('--disk', '0'), ('--cpu', '2', '--cpu', '3'),
                      ('--network', 'shared'), ('--runtime', 'containerd'), ('--kubernetes', 'true')]:
        before = config.read_bytes()
        assert not run('vm', 'configure', '--profile', 'test', '--yes', *arguments)['ok']
        assert config.read_bytes() == before

    # Existing advanced settings are preserved by resource-only configuration.
    share = root / 'share'
    share.mkdir()
    advanced = write('advanced', f'''cpus: 2
mountHome: false
mountInotify: true
rosetta: true
nestedVirtualization: true
sshAgent: true
docker:
  daemonJson: '{{"features":{{"buildkit":true}}}}'
mounts:
  - location: "{share}"
    mountPoint: /workspace
    writable: true
provision:
  - command: echo ready
    stage: system
    timeoutSeconds: 30
    mode: fail
''')
    assert run('vm', 'configure', '--profile', 'advanced', '--cpu', '4', '--yes')['ok']
    for required in ('mountInotify: true', 'rosetta: true', 'nestedVirtualization: true',
                     'sshAgent: true', 'echo ready', '/workspace', str(share), 'buildkit'):
        assert required in advanced.read_text(), required

    # CASES are populated from the existing strict-parser regression fixtures.
    cases = {'unknown-key': 'cpus: 4\nunknown: true\n',
     'duplicate-key': 'cpus: 4\ncpus: 5\n',
     'yaml-anchor': 'cpus: &cpu 4\n',
     'yaml-alias': 'cpus: *cpu\n',
     'yaml-tag': 'cpus: !!int 4\n',
     'yaml-merge': 'base: &base { cpus: 4 }\n<<: *base\n',
     'quoted-bool': 'mountHome: "true"\n',
     'wrong-type': 'mounts: true\n',
     'invalid-mount': 'mounts:\n'
                      '  - location: relative\n'
                      '    mountPoint: /workspace\n'
                      '    writable: false\n',
     'invalid-hook': 'provision:\n'
                     '  - command: echo ready\n'
                     '    stage: invalid\n'
                     '    timeoutSeconds: 60\n'
                     '    mode: fail\n',
     'removed-network-key': 'network:\n  mode: shared\n',
     'quoted-mount-inotify': 'mountInotify: "true"\n',
     'no-writable-mount-inotify': 'mountHome: false\nmountInotify: true\n',
     'invalid-docker-json': 'docker:\n  daemonJson: "["\n',
     'invalid-docker-key': 'docker:\n  daemonJson: "{\\"containerd\\":\\"/other.sock\\"}"\n'}
    for name, text in cases.items():
        path = write(name, text)
        before = path.read_bytes()
        value = run('vm', 'status', '--profile', name)
        assert not value['ok'], (name, value)
        assert path.read_bytes() == before
        assert not run('vm', 'configure', '--profile', name, '--cpu', '4', '--yes')['ok']
        assert path.read_bytes() == before

    legacy = root / '.hamn/legacy'
    legacy.mkdir()
    (legacy / 'hamn.conf').write_text('runtime=containerd\n')
    assert not run('vm', 'status', '--profile', 'legacy')['ok']
    assert not run('vm', 'start', '--profile', 'legacy', '--yes')['ok']
    assert not (legacy / 'config.yaml').exists()
    for name in cases:
        (root / '.hamn' / name / 'config.yaml').unlink()
    assert run('vm', 'create', '--profile', 'deleted', '--yes')['ok']
    disk = root / '.hamn/deleted/disk.img'
    disk.write_bytes(b'Docker data sentinel')
    assert not run('vm', 'delete', '--profile', 'deleted')['ok']
    assert run('vm', 'delete', '--profile', 'deleted', '--yes')['ok']
    assert disk.read_bytes() == b'Docker data sentinel'
    assert (disk.parent / 'deleted').is_file()
    assert all(row['name'] != 'deleted' for row in run('vm', 'list')['data'])
print('strict profiles, advanced settings preservation and soft deletion: passed')
