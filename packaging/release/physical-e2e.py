#!/usr/bin/env python3
"""Run exact candidate bytes; successful evidence requires every physical check.

Requires HAMN_CANDIDATE_DIR, HAMN_E2E_OUTPUT, HAMN_E2E_CONTEXT,
HAMN_E2E_KUBECONFIG, HAMN_LEGACY_BINARY, HAMN_LEGACY_BINARY_SHA256,
and HAMN_LEGACY_RUNNING_FIXTURE / HAMN_LEGACY_STOPPED_FIXTURE directories.
Legacy fixtures contain a stopped profile plus expected.json, captured with the
legacy binary after creating Docker sentinel objects and K3s data. No user VM
or kubeconfig is changed; all profile disks are cloned into an isolated HOME.
"""
import hashlib
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import sys
import tarfile
import tempfile

from physical_contract import CHECKS, read_json, sha256, validate
from physical_runtime import Runtime, run


def owned(path):
    path = Path(path)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_nlink != 1:
        raise ValueError('unsafe physical validation input: ' + str(path))
    return path


def unpack(archive, destination):
    with tarfile.open(owned(archive)) as bundle:
        entries = bundle.getmembers()
        if not entries or len(entries) > 10000 or sum(entry.size for entry in entries) > 512 * 1024 * 1024:
            raise ValueError('excessive candidate archive')
        names = set()
        for entry in entries:
            path = Path(entry.name)
            if path.is_absolute() or '..' in path.parts or entry.name in names or not (entry.isfile() or entry.isdir()):
                raise ValueError('unsafe candidate archive entry')
            names.add(entry.name)
        bundle.extractall(destination, members=entries, filter='data')
    roots = list(destination.iterdir())
    if len(roots) != 1 or not roots[0].is_dir():
        raise ValueError('candidate must have exactly one archive root')
    return roots[0]


def fixture(source, destination, state, legacy_hash):
    source = Path(source).resolve(strict=True)
    expected = read_json(owned(source / 'expected.json'))
    if set(expected) != {'k3sState', 'docker'} or expected['k3sState'] != state:
        raise ValueError('legacy fixture state does not match required case')
    destination.mkdir(mode=0o700)
    digest = hashlib.sha256(legacy_hash.encode())
    files = ['disk.img', 'config.yaml', 'id_ed25519', 'id_ed25519.pub', 'efi-vars.bin', 'machine-id.bin', 'mac-addr']
    for name in files:
        path = owned(source / name)
        digest.update(name.encode())
        digest.update(sha256(path).encode())
        run(['/bin/cp', '-c', path, destination / name])
    config = (destination / 'config.yaml').read_text()
    if 'mounts: []' not in config or 'provision: []' not in config or 'kubernetes:' not in config:
        raise ValueError('legacy fixture must have no custom mounts or provision commands')
    (destination / 'config.yaml').write_text(config.replace('mountHome: true', 'mountHome: false'))
    digest.update(json.dumps(expected, sort_keys=True).encode())
    return expected['docker'], digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(prog='physical-e2e.sh', description=__doc__)
    parser.parse_args()
    required = ['HAMN_CANDIDATE_DIR', 'HAMN_E2E_OUTPUT', 'HAMN_E2E_CONTEXT', 'HAMN_E2E_KUBECONFIG',
                'HAMN_LEGACY_BINARY', 'HAMN_LEGACY_BINARY_SHA256', 'HAMN_LEGACY_RUNNING_FIXTURE', 'HAMN_LEGACY_STOPPED_FIXTURE']
    if any(not os.environ.get(key) for key in required):
        raise ValueError('missing physical validator inputs: ' + ', '.join(key for key in required if not os.environ.get(key)))
    if run(['uname', '-m']).strip() != 'arm64':
        raise ValueError('physical Apple Silicon validator required')
    docker = shutil.which('docker')
    if not docker or not shutil.which('kubectl'):
        raise ValueError('external Docker CLI and kubectl fixture tools are required')
    candidate_dir = Path(os.environ['HAMN_CANDIDATE_DIR']).resolve(strict=True)
    candidate_path = candidate_dir / 'candidate.json'
    candidate = read_json(candidate_path)
    artifacts = {entry['name']: entry['sha256'] for entry in candidate['artifacts']}
    for name, digest in artifacts.items():
        if Path(name).name != name or sha256(owned(candidate_dir / name)) != digest:
            raise ValueError('candidate artifact digest mismatch')
    host_name = next(name for name in artifacts if name.endswith('-darwin-arm64.tar.gz'))
    guest_name = next(name for name in artifacts if name.endswith('-ubuntu-24.04-arm64.img'))
    legacy_binary = owned(os.environ['HAMN_LEGACY_BINARY']).resolve()
    legacy_hash = sha256(legacy_binary)
    if legacy_hash != os.environ['HAMN_LEGACY_BINARY_SHA256']:
        raise ValueError('legacy binary digest mismatch')
    output = Path(os.environ['HAMN_E2E_OUTPUT']).absolute()
    if output.exists():
        raise ValueError('physical evidence output already exists')
    work = Path(tempfile.mkdtemp(prefix='hamn-physical-e2e-'))
    runtime, profiles = None, []
    checks, legacy_results = set(), {}
    try:
        unpacked = work / 'candidate'
        unpacked.mkdir()
        root = unpack(candidate_dir / host_name, unpacked)
        binary = owned(root / 'bin/hamn')
        run(['bash', root / 'scripts/check-host-binary.sh', binary])
        if run([binary, '--version']).strip() != 'hamn ' + candidate['version'].removeprefix('v'):
            raise ValueError('candidate executable version mismatch')
        binary_hash = sha256(binary)
        checks.add('singleBinary')
        home = work / 'home'
        cache = home / '.hamn/cache'
        cache.mkdir(parents=True, mode=0o700)
        runtime = Runtime(binary, home, docker)
        digest = artifacts[guest_name]
        image_name = 'hamn-guest-' + digest + '.img'
        run(['/bin/cp', '-c', candidate_dir / guest_name, cache / image_name])
        (cache / (image_name + '.verified')).write_text(digest)
        (cache / 'guest-image.json').write_text(json.dumps({'schemaVersion': 1, 'file': image_name, 'sha256': digest}))
        for profile in ['default', 'second']:
            runtime.call('vm', 'create', profile=profile, yes=True, cpu=2, memory=2)
            profiles.append(profile)
            config = home / '.hamn' / profile / 'config.yaml'
            config.write_text(config.read_text().replace('mountHome: true', 'mountHome: false'))
            runtime.call('vm', 'start', profile=profile, yes=True)
            runtime.call('docker', 'containers', 'list', profile=profile)
        checks.update(['multipleProfiles', 'dockerWithoutCli', 'vmLifecycle'])
        runtime.engine('run', '-d', '--name', 'hamn-e2e', 'busybox:1.37', 'sh', '-c', 'echo hamn-e2e; sleep 3600')
        for action in ['inspect', 'logs', 'stats', 'stop', 'start', 'restart']:
            runtime.call('docker', 'containers', action, 'hamn-e2e', **({'yes': True} if action in ['stop', 'start', 'restart'] else {}))
        runtime.call('docker', 'containers', 'stop', 'hamn-e2e', yes=True)
        runtime.call('docker', 'containers', 'delete', 'hamn-e2e', yes=True)
        checks.update(['dockerApi', 'externalDockerSocket'])
        runtime.terminal()
        assert runtime.call('vm', 'status')['state'] == 'running'
        checks.update(['tuiTerminalRestore', 'vmSurvivesTuiExit'])
        runtime.call('vm', 'stop', yes=True)
        runtime.call('vm', 'start', yes=True)
        for state in ['running', 'stopped']:
            profile = 'retire-' + state
            before, source_hash = fixture(os.environ['HAMN_LEGACY_' + state.upper() + '_FIXTURE'], home / '.hamn' / profile, state, legacy_hash)
            profiles.append(profile)
            if state == 'running':
                run([legacy_binary, 'start', '--profile', profile, '--template=false'], dict(os.environ, HOME=str(home)))
                runtime.terminal(retiring=profile)
            else:
                runtime.call('vm', 'start', profile=profile, yes=True)
            runtime.verify_retired(profile)
            after = runtime.snapshot(profile)
            if before != after:
                raise ValueError('Docker identifiers or volume bytes changed during ' + state + ' retirement')
            legacy_results[state] = {'before': before, 'after': after, 'k3sRemoved': True, 'journalComplete': True, 'sourceSha256': source_hash}
        checks.update(['k3sRunningRetirement', 'k3sStoppedRetirement', 'dockerDataPreserved'])
        kube_path = work / 'kubernetes.json'
        run([sys.executable, root / 'packaging/release/external-kubernetes-e2e.py', '--hamn', binary,
             '--context', os.environ['HAMN_E2E_CONTEXT'], '--kubeconfig', os.environ['HAMN_E2E_KUBECONFIG'], '--output', kube_path], timeout=1200)
        kubernetes = read_json(kube_path)
        checks.update(['externalKubernetes', 'kubeconfigUnchanged'])
        assert sha256(binary) == binary_hash
    finally:
        if runtime:
            runtime.stop(profiles)
        shutil.rmtree(work)
    checks.add('cleanup')
    assert checks == CHECKS
    evidence = {'schemaVersion': 2, 'kind': 'hamn-physical-validation-evidence', 'validationMode': 'physical-apple-silicon',
        **{key: candidate[key] for key in ['tag', 'commit', 'sourceTree']},
        'workflow': {'run': os.environ.get('GITHUB_RUN_ID', 'local'), 'attempt': os.environ.get('GITHUB_RUN_ATTEMPT', 'local')},
        'candidate': {'candidateJsonSha256': sha256(candidate_path), 'checksumsSha256': sha256(candidate_dir / 'SHA256SUMS'), 'artifacts': artifacts},
        'checks': dict.fromkeys(checks, True), 'legacy': legacy_results, 'kubernetes': kubernetes}
    with output.open('x') as destination:
        json.dump(evidence, destination, sort_keys=True)
        destination.write('\n')
    validate(candidate_path, candidate_dir / 'SHA256SUMS', output, evidence['workflow']['run'], evidence['workflow']['attempt'])


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_args: sys.exit(143))
    main()
