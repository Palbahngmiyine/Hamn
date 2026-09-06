#!/usr/bin/env python3
"""Opt-in Apple Silicon integration. Owns an isolated HOME; never uses user contexts."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / 'packaging/release'))
from physical_runtime import Runtime, run


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def prepare(binary, cache, existing):
    if existing:
        root = Path(existing).absolute()
        owner = json.loads((root / 'ownership.json').read_text())
        assert owner['workspace'] == str(REPO) and owner['profile'] == 'verify'
        assert Path(owner['home']).resolve() == (root / 'home').resolve()
    else:
        root = Path(tempfile.mkdtemp(prefix='hamn-workspace-live-', dir='/tmp'))
        home = root / 'home'
        target = home / '.hamn/cache'
        target.mkdir(parents=True, mode=0o700)
        manifest = json.loads((cache / 'guest-image.json').read_text())
        # Copy only the selected, locally verified signed cache; start verifies it again.
        images = list(cache.glob('hamn-guest-*.img.verified'))
        assert len(images) == 1, 'provide a cache containing one signed guest image'
        image = Path(str(images[0])[:-len('.verified')])
        for source in (image, images[0], cache / 'guest-image.json'):
            run(['/bin/cp', '-c', source, target / source.name])
        owner = {'owner':'Codex workspace integration test', 'workspace':str(REPO),
                 'profile':'verify', 'home':str(home), 'guestImageSha256':digest(image)}
        (root / 'ownership.json').write_text(json.dumps(owner, indent=2))
    (root / 'binary-sha256.txt').write_text(digest(binary) + '\n')
    runtime = Runtime(binary, root / 'home', shutil.which('docker'))
    runtime.environment['PATH'] = '/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin'
    runtime.environment['TERM'] = 'xterm-256color'
    docker_config = runtime.home / '.docker'
    docker_config.mkdir(exist_ok=True)
    (docker_config / 'config.json').write_text(json.dumps({'cliPluginsExtraDirs':['/opt/homebrew/lib/docker/cli-plugins']}))
    return root, runtime


def snapshot(runtime):
    def docker(*args): return runtime.engine(*args, profile='verify')
    return {'containers':sorted(docker('ps', '-aq', '--no-trunc').split()),
            'images':sorted(set(docker('images', '-q', '--no-trunc').split())),
            'volumes':sorted(docker('volume', 'ls', '-q').split()),
            'data':docker('exec', 'hamn-workspace-sentinel', 'sha256sum', '/data/sentinel').split()[0]}


def recovery(root, runtime):
    def docker(*args): return runtime.engine(*args, profile='verify')
    if 'hamn-workspace-sentinel' not in docker('ps', '-a', '--format', '{{.Names}}').split():
        docker('pull', 'busybox:1.37')
        docker('volume', 'create', '--label', 'io.hamn.test=workspace', 'hamn-workspace-data')
        docker('run', '--rm', '-v', 'hamn-workspace-data:/data', 'busybox:1.37', 'sh', '-c',
               'printf "hamn-preservation-proof-20260907\\n" > /data/sentinel')
        docker('run', '-d', '--name', 'hamn-workspace-sentinel', '--restart', 'always',
               '--label', 'io.hamn.test=workspace', '-v', 'hamn-workspace-data:/data',
               'busybox:1.37', 'sh', '-c', 'while :; do sleep 30; done')
    before = snapshot(runtime)
    baseline = root / 'before.json'
    if baseline.exists(): assert before == json.loads(baseline.read_text()), 'earlier recovery changed data'
    baseline.write_text(json.dumps(before, indent=2))
    pid = (runtime.home / '.hamn/verify/vmrun.pid').read_text()
    runtime.ssh('''test ! -e /usr/local/libexec/hamn/configure-k3s
printf '{"version":1,"stage":"complete"}' > /var/lib/hamn/k3s-retirement-v1.json
chmod 600 /var/lib/hamn/k3s-retirement-v1.json''', profile='verify')
    results = []
    for legacy, token in [(False, 'c' * 32), (True, 'd' * 32)]:
        path = '/var/lib/hamn/deployment-transactions/' + token
        runtime.ssh(f'flock /run/hamn-deployment.lock bash /usr/local/libexec/hamn/guest-deployment-transaction begin {token}', profile='verify')
        if legacy: runtime.ssh(f'rm {path}/provenance.json', profile='verify')
        (runtime.home / '.hamn/verify/docker.sock').unlink()
        result = runtime.call('vm', 'start', profile='verify', yes=True)
        assert result['dockerStatus'] == 'ready' and result['lastOperation']['status'] == 'completed'
        assert not result['lastOperation']['startedVm']
        assert (runtime.home / '.hamn/verify/vmrun.pid').read_text() == pid
        runtime.ssh(f'test ! -e {path}; test ! -e /usr/local/libexec/hamn/configure-k3s', profile='verify')
        assert snapshot(runtime) == before
        results.append({'legacy':legacy, 'status':result})
        print('PASS: socket recovery and data preservation; legacy =', legacy, flush=True)
    (root / 'recovery-results.json').write_text(json.dumps(results, indent=2))
    runtime.call('vm', 'stop', profile='verify', yes=True)
    restarted = runtime.call('vm', 'start', profile='verify', yes=True)
    assert restarted['dockerStatus'] == 'ready'
    assert snapshot(runtime) == before
    print('PASS: real stop/start preserved container, image, volume and sentinel', flush=True)


def cli_extensions(root, runtime):
    docker = lambda *args: runtime.engine(*args, profile='verify')
    build = root / 'build-context'
    build.mkdir(exist_ok=True)
    (build / 'Dockerfile').write_text('FROM busybox:1.37\nRUN printf buildx-proof > /proof\n')
    docker('buildx', 'build', '--load', '-t', 'hamn-workspace-build:verify', str(build))
    assert docker('run', '--rm', 'hamn-workspace-build:verify', 'cat', '/proof').strip() == 'buildx-proof'
    compose = root / 'compose.yaml'
    compose.write_text('services:\n  probe:\n    image: busybox:1.37\n    command: ["sh", "-c", "echo compose-proof; sleep 300"]\n')
    try:
        docker('compose', '-p', 'hamn-workspace-proof', '-f', str(compose), 'up', '-d')
        assert 'compose-proof' in docker('compose', '-p', 'hamn-workspace-proof', '-f', str(compose), 'logs')
    finally:
        docker('compose', '-p', 'hamn-workspace-proof', '-f', str(compose), 'down')
    (root / 'cli-versions.txt').write_text(docker('version') + docker('compose', 'version') + docker('buildx', 'version'))
    print('PASS: installed Compose and buildx against isolated Docker', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', help='resume only a workspace-owned integration root')
    parser.add_argument('--cache', type=Path, default=Path.home() / '.hamn/cache')
    parser.add_argument('--binary', type=Path, default=REPO / 'build/hamn')
    parser.add_argument('--keep-running', action='store_true', help='retain owned VM for further tests')
    args = parser.parse_args()
    root, runtime = prepare(args.binary.resolve(), args.cache, args.root)
    print('Evidence and owned runtime:', root, flush=True)
    try:
        runtime.call('vm', 'start', profile='verify', yes=True, cpu=4, memory=6, disk=60)
        recovery(root, runtime)
        cli_extensions(root, runtime)
    finally:
        if not args.keep_running: runtime.stop(['verify'])


if __name__ == '__main__':
    main()
