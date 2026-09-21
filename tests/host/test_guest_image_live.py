#!/usr/bin/env python3
"""Opt-in same-contract physical tests for locally built guest images.

Each image gets an isolated HOME and a single 2-CPU/2-GiB VM. The private cache
marker is local test authorization by exact digest, NOT a release signature or
attestation. No user profiles, home sharing, source provisioning, or Rosetta
installation is used. A failed cleanup stops the sequence and retains evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import sys
import tempfile
import time
import uuid

from test_workspace_live import REPO, cli_extensions, prepare
from physical_runtime import run


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def inputs(args):
    report = json.loads(args.size_report.read_text())
    assert report['sourceRevision'] == args.source_revision
    assert re.fullmatch('[0-9a-f]{40}', args.source_revision)
    assert report['virtualBytes'] == 8 * 1024**3
    values = [('baseline', args.baseline, report['baselineSha256'], report['baselineCompressedBytes'], args.size_report),
              ('optimized', args.optimized, report['imageSha256'], report['compressedBytes'], args.size_report)]
    variants = json.loads(args.variations.read_text())
    assert variants['sourceRevision'] == args.source_revision
    assert variants['baselineSha256'] == report['baselineSha256']
    assert variants['baselineSizeReportSha256'] == digest(args.size_report)
    assert len(variants['variants']) == 2
    for variant in variants['variants']:
        path = args.variations.parent / variant['image']
        size_report = args.variations.parent / variant['sizeReport']
        detail = json.loads(size_report.read_text())
        assert detail['sourceRevision'] == args.source_revision and detail['baselineSha256'] == report['baselineSha256']
        assert detail['imageSha256'] == variant['imageSha256']
        assert variant['structuralChecks'] == 'passed'
        values.append(('case-' + str(variant['case']), path, variant['imageSha256'], detail['compressedBytes'], size_report))
    for label, path, sha, size, _ in values:
        assert path.is_file() and not path.is_symlink() and path.stat().st_size == size
        assert digest(path) == sha, 'image identity mismatch: ' + label
    return values


def config_bool(config, name, value):
    text = config.read_text()
    pattern = r'^' + re.escape(name) + r': (?:true|false)$'
    assert len(re.findall(pattern, text, re.MULTILINE)) == 1, 'invalid boolean configuration: ' + name
    config.write_text(re.sub(pattern, name + ': ' + ('true' if value else 'false'), text, flags=re.MULTILINE))


def cni_script(token, arm_image):
    # Every namespace, bridge, allocation directory and temporary file is owned
    # by this generated UUID. DEL runs even if ADD partially failed.
    network, bridge, namespace = 'hamn-proof-' + token, 'hp' + token[:12], 'hp-' + token
    assert re.fullmatch('[0-9a-f]{32}', token)
    assert re.fullmatch('sha256:[0-9a-f]{64}', arm_image)
    third = 16 + int(token[:2], 16) % 220
    config = json.dumps({'cniVersion': '0.4.0', 'name': network, 'type': 'bridge', 'bridge': bridge,
        'isGateway': True, 'ipMasq': False, 'ipam': {'type': 'host-local', 'subnet': f'10.237.{third}.0/24'}})
    loopback = json.dumps({'cniVersion': '0.4.0', 'name': network + '-lo', 'type': 'loopback'})
    return f'''set -euo pipefail
network={shlex.quote(network)}
bridge={shlex.quote(bridge)}
namespace={shlex.quote(namespace)}
config={shlex.quote(config)}
loopback={shlex.quote(loopback)}
! ip link show "$bridge" >/dev/null 2>&1
! ip netns list | awk '{{print $1}}' | grep -Fx "$namespace"
test ! -e "/var/lib/cni/networks/$network"
work=$(mktemp -d /tmp/hamn-cni-proof.XXXXXX)
ns_created=0
attempted=0
tool_attempted=0
cleanup() {{
    status=$?
    trap - EXIT
    set +e
    failed=0
    if [ "$attempted" = 1 ]; then
        printf '%s' "$loopback" | CNI_COMMAND=DEL CNI_IFNAME=lo /opt/cni/bin/loopback >"$work/loop-del" 2>&1 || failed=1
        printf '%s' "$config" | CNI_COMMAND=DEL CNI_IFNAME=eth0 /opt/cni/bin/bridge >"$work/bridge-del" 2>&1 || failed=1
    fi
    if [ "$ns_created" = 1 ]; then ip netns delete "$namespace" || failed=1; fi
    if ip link show "$bridge" >/dev/null 2>&1; then ip link delete "$bridge" || failed=1; fi
    if [ -d "/var/lib/cni/networks/$network" ]; then
        if find "/var/lib/cni/networks/$network" -type f -name '10.*' | grep -q .; then failed=1; fi
        rm -rf "/var/lib/cni/networks/$network"
    fi
    if [ "$tool_attempted" = 1 ]; then
        # Re-observe after a timeout: create can succeed before its output is
        # received. Exact generated name plus owner label excludes other work.
        tool_container=$(timeout 30 docker ps -aq --no-trunc \\
            --filter 'name=^/hamn-cni-ping-{token}$' --filter 'label=io.hamn.test={token}') || failed=1
        if [ -n "$tool_container" ]; then
            if [[ "$tool_container" =~ ^[0-9a-f]{{64}}$ ]]; then
                timeout 30 docker rm "$tool_container" >/dev/null || failed=1
            else failed=1; fi
        fi
    fi
    cat "$work/loop-del" "$work/bridge-del" 2>/dev/null || true
    rm -rf "$work"
    [ "$failed" = 0 ] || status=1
    if [ "$status" = 0 ]; then printf 'CNI_ADD_PING_DEL_OK %s\\n' "$namespace"; fi
    exit "$status"
}}
trap cleanup EXIT
# The already executed ARM64 image supplies the test-only ICMP observer. This
# keeps minimal/standard comparisons independent of optional host ping tools
# without installing or repairing any guest package.
tool_attempted=1
tool_container=$(timeout 30 docker create --platform linux/arm64 \\
    --name hamn-cni-ping-{token} --label io.hamn.test={token} {arm_image} true)
[[ "$tool_container" =~ ^[0-9a-f]{{64}}$ ]]
timeout 30 docker cp "$tool_container:/bin/busybox" "$work/busybox"
test -f "$work/busybox" && test ! -L "$work/busybox"
chmod 0755 "$work/busybox"
printf 'CNI_ICMP_TOOL_IMAGE %s\\n' {arm_image}
sha256sum "$work/busybox"
export CNI_CONTAINERID={token} CNI_NETNS=/var/run/netns/$namespace CNI_PATH=/opt/cni/bin
ip netns add "$namespace"
ns_created=1
attempted=1
printf '%s' "$loopback" | CNI_COMMAND=ADD CNI_IFNAME=lo /opt/cni/bin/loopback
printf '%s' "$config" | CNI_COMMAND=ADD CNI_IFNAME=eth0 /opt/cni/bin/bridge
ip -n "$namespace" -j address show eth0
ip netns exec "$namespace" "$work/busybox" ping -c 2 -W 2 10.237.{third}.1
ip netns exec "$namespace" "$work/busybox" ping -c 2 -W 2 127.0.0.1
'''


def test_image(args, value):
    label, image, sha256, size, size_report = value
    root = Path(tempfile.mkdtemp(prefix='hamn-image-live-' + label + '-', dir='/tmp')).resolve()
    home = root / 'home'; home.mkdir(mode=0o700)
    runtime_root = home / '.hamn'; runtime_root.mkdir(mode=0o700)
    cache = runtime_root / 'cache'; cache.mkdir(mode=0o755)
    selected = cache / ('hamn-guest-' + sha256 + '.img')
    run(['/bin/cp', '-c', image, selected], timeout=60)
    selected.with_suffix('.img.verified').write_text(sha256 + '\n')
    (cache / 'guest-image.json').write_text(json.dumps({'schemaVersion': 1, 'file': selected.name, 'sha256': sha256}))
    ownership = {'owner': 'Hamn locally built image runtime test', 'workspace': str(REPO),
        'profile': 'verify', 'home': str(home), 'guestImageSha256': sha256,
        'trust': 'explicit local test digest; not release attestation'}
    (root / 'ownership.json').write_text(json.dumps(ownership, indent=2))
    _, runtime = prepare(args.binary.resolve(), None, root)
    token = uuid.uuid4().hex
    container, volume = 'hamn-proof-' + token[:12], 'hamn-data-' + token[:12]
    evidence = {'label': label, 'sourceRevision': args.source_revision, 'imageSHA256': sha256,
        'compressedBytes': size, 'sizeReportSHA256': digest(size_report),
        'candidateBinarySHA256': digest(runtime.binary), 'trust': ownership['trust'],
        'cpus': 2, 'memoryGiB': 2, 'mountHome': False, 'checks': {}, 'ownershipRoot': str(root)}
    output = args.output_directory / (label + '.json')
    created, stopped = False, False
    stage = 'create'
    def save(): output.write_text(json.dumps(evidence, indent=2) + '\n')
    def mark(name, details):
        evidence['checks'][name] = details; save()
        print(f'{label}: PASS {name}', flush=True)
    def docker(*words): return runtime.engine(*words, profile='verify')
    def state():
        return {'containerId': docker('inspect', '--format', '{{.Id}}', container).strip(),
            'volume': json.loads(docker('volume', 'inspect', volume))[0],
            'sentinelSHA256': docker('exec', container, 'sha256sum', '/data/sentinel').split()[0],
            'images': sorted(set(docker('images', '-q', '--no-trunc').split()))}
    try:
        runtime.call('vm', 'create', profile='verify', yes=True, cpu=2, memory=2, disk=24)
        created = True
        config = home / '.hamn/verify/config.yaml'
        config_bool(config, 'mountHome', False); config_bool(config, 'rosetta', False)
        stage = 'initial-boot'
        start = time.monotonic(); boot = runtime.call('vm', 'start', profile='verify', yes=True)
        status = runtime.call('vm', 'status', profile='verify')
        assert boot['dockerStatus'] == 'ready' and status['mountHome'] is False and status['rosetta'] is False
        mark('boot', {'elapsedSeconds': time.monotonic() - start, 'status': status})
        stage = 'container-runtimes'
        versions = runtime.ssh('systemctl is-active containerd docker\ncontainerd --version\nrunc --version\nctr namespaces list -q', profile='verify')
        assert 'moby' in versions
        qemu = runtime.ssh('cat /proc/sys/fs/binfmt_misc/qemu-x86_64\ntest ! -e /proc/sys/fs/binfmt_misc/hamn-rosetta', profile='verify')
        assert qemu.startswith('enabled\n') and 'qemu' in qemu
        docker('pull', '--platform=linux/arm64', 'busybox:1.37')
        arm = docker('image', 'inspect', '--format', '{{.Id}}', 'busybox:1.37').strip()
        assert docker('run', '--rm', '--platform=linux/arm64', arm, 'uname', '-m').strip() == 'aarch64'
        docker('pull', '--platform=linux/amd64', 'busybox:1.37')
        amd = docker('image', 'inspect', '--format', '{{.Id}}', 'busybox:1.37').strip()
        assert docker('run', '--rm', '--platform=linux/amd64', amd, 'uname', '-m').strip() == 'x86_64'
        docker('tag', arm, 'busybox:1.37')
        mark('arm64-and-qemu-amd64', {'armImageId': arm, 'amdImageId': amd, 'qemuRegistration': qemu, 'runtimeVersions': versions})
        docker('volume', 'create', '--label', 'io.hamn.test=' + token, volume)
        docker('run', '--rm', '--mount', f'type=volume,src={volume},dst=/data', arm,
               'sh', '-c', 'printf %s ' + token + ' > /data/sentinel')
        sentinel_id = docker('run', '-d', '--name', container, '--restart', 'always', '--label', 'io.hamn.test=' + token,
                            '--mount', f'type=volume,src={volume},dst=/data', arm, 'sh', '-c', 'while :; do sleep 30; done').strip()
        rows = runtime.call('docker', 'containers', 'list', profile='verify')
        assert any(row['Id'] == sentinel_id for row in rows)
        tasks = runtime.ssh('ctr -n moby tasks list', profile='verify')
        assert sentinel_id in tasks
        assert docker('inspect', '--format', '{{.HostConfig.Runtime}}', container).strip() == 'runc'
        mark('docker-api-containerd-runc', {'containerId': sentinel_id, 'containerdTasks': tasks,
                                          'dockerVersion': docker('version'), 'apiRows': rows})
        stage = 'compose-buildx'
        cli_extensions(root, runtime)
        mark('compose-buildx', {'versions': (root / 'cli-versions.txt').read_text()})
        stage = 'cni'
        cni = runtime.ssh(cni_script(token, arm), profile='verify')
        assert 'CNI_ADD_PING_DEL_OK hp-' + token in cni
        mark('cni-bridge-host-local-loopback', {'output': cni})
        stage = 'stop-start-preservation'
        before = state()
        runtime.call('vm', 'stop', profile='verify', yes=True)
        runtime.call('vm', 'start', profile='verify', yes=True)
        assert state() == before
        mark('stop-start-preservation', before)
        stage = 'rosetta'
        runtime.call('vm', 'stop', profile='verify', yes=True)
        config_bool(config, 'rosetta', True)
        try:
            runtime.call('vm', 'start', profile='verify', yes=True)
        except Exception as error:
            evidence['checks']['rosetta'] = {'passed': False, 'error': str(error), 'systemInstallationAttempted': False}
            raise
        rosetta = runtime.ssh('cat /proc/sys/fs/binfmt_misc/hamn-rosetta\ntest ! -e /proc/sys/fs/binfmt_misc/qemu-x86_64', profile='verify')
        assert rosetta.startswith('enabled\n') and '/mnt/hamn-rosetta/rosetta' in rosetta
        assert docker('run', '--rm', '--platform=linux/amd64', amd, 'uname', '-m').strip() == 'x86_64'
        assert state() == before
        mark('rosetta', {'passed': True, 'registration': rosetta, 'systemInstallationAttempted': False})
        evidence['buildTools'] = runtime.ssh('for tool in gcc make; do if command -v "$tool"; then "$tool" --version | head -n 1; else printf "%s absent\\n" "$tool"; fi; done', profile='verify')
        evidence['passed'] = True
    except KeyboardInterrupt:
        evidence.update(passed=False, failedStage=stage, interrupted=True,
                        error='Image sequence interrupted; remaining images were not run')
        raise
    except Exception as error:
        evidence.update(passed=False, failedStage=stage, error=str(error))
        print(f'{label}: FAIL {stage}: {error}', flush=True)
    finally:
        if created:
            try:
                runtime.stop(['verify']); stopped = True
                evidence['cleanup'] = 'VM stopped and ownership verified'
            except Exception as error:
                evidence['cleanupError'] = str(error)
        else: stopped = True
        for name in ('vmrun.log', 'serial.log', 'provision.log'):
            log = home / '.hamn/verify/logs' / name
            if log.is_file(): shutil.copy2(log, args.output_directory / (label + '-' + name))
        save()
        if stopped and evidence.get('passed'):
            assert json.loads((root / 'ownership.json').read_text()) == ownership
            shutil.rmtree(root)
            evidence['cleanup'] = 'VM stopped; private HOME, profile, cache and SSH keys removed'; save()
        elif stopped:
            evidence['retainedStoppedRoot'] = str(root); save()
        else:
            raise RuntimeError('owned VM cleanup failed; sequence stopped: ' + str(root))
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--optimized', type=Path, required=True)
    parser.add_argument('--size-report', type=Path, required=True)
    parser.add_argument('--variations', type=Path, required=True)
    parser.add_argument('--source-revision', required=True)
    parser.add_argument('--output-directory', type=Path, required=True)
    parser.add_argument('--only', choices=('baseline', 'optimized', 'case-0', 'case-1'))
    args = parser.parse_args()
    harness_sha256 = digest(Path(__file__))
    values = inputs(args)
    args.output_directory.mkdir(mode=0o700)
    results = []
    for value in values:
        if args.only and value[0] != args.only: continue
        results.append(test_image(args, value))
        (args.output_directory / 'summary.json').write_text(json.dumps({'schemaVersion': 1,
            'harnessSHA256': harness_sha256, 'sourceRevision': args.source_revision,
            'allRequiredPassed': len(results) == 4 and all(item.get('passed') for item in results),
            'pending': [item[0] for item in values if item[0] not in {result['label'] for result in results}],
            'results': results}, indent=2) + '\n')
        if not results[-1].get('passed'):
            # A common runtime defect needs diagnosis, not three repetitions.
            break
    if not all(item.get('passed') for item in results): raise SystemExit(1)


if __name__ == '__main__': main()
