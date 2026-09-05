#!/usr/bin/env python3
"""Validate a supplied Hamn binary in a disposable namespace of an explicit cluster.

kubectl owns fixture setup/cleanup only. Every operation under test uses Hamn.
The original kubeconfig bytes and current-context are never changed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hamn', required=True, type=Path)
    parser.add_argument('--context', required=True)
    parser.add_argument('--kubeconfig', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--host-network', action='store_true', help='Use host networking for API/log tests when the test cluster CNI is unavailable; this does not validate Pod networking')
    args = parser.parse_args()
    binary = args.hamn.resolve(strict=True)
    if args.output.exists():
        raise RuntimeError('evidence output already exists')
    namespace = 'hamn-e2e-' + uuid.uuid4().hex[:16]
    config_paths = [args.kubeconfig] if args.kubeconfig else [Path(p) for p in
        os.environ.get('KUBECONFIG', str(Path.home() / '.kube/config')).split(os.pathsep) if p]
    before = {path: path.read_bytes() for path in config_paths}
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    kube = ['kubectl', '--context', args.context, '--request-timeout=30s']
    hamn = [str(binary), '--headless', '--context', args.context, '--namespace', namespace, '--timeout', '60']
    if args.kubeconfig:
        kube += ['--kubeconfig', str(args.kubeconfig)]
        hamn += ['--kubeconfig', str(args.kubeconfig)]
    checks = []
    created = False

    def run(command, payload=None, timeout=90):
        result = subprocess.run(command, input=payload, text=True, capture_output=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError(f'{command[0]} failed ({result.returncode}): {result.stderr[-2048:]} {result.stdout[-2048:]}')
        return result.stdout

    def operation(*words, **flags):
        command = hamn + list(words)
        for key, value in flags.items():
            command += ['--' + key.replace('_', '-')]
            if value is not True:
                command.append(str(value))
        result = json.loads(run(command))
        if result.get('schemaVersion') != 1 or result.get('ok') is not True:
            raise RuntimeError(f'invalid Hamn response: {result}')
        checks.append(' '.join(words[:3]))
        return result['data']

    try:
        run(kube + ['create', 'namespace', namespace])
        created = True
        items = []
        for kind, name in [('Deployment', 'deployment'), ('StatefulSet', 'statefulset'), ('DaemonSet', 'daemonset')]:
            labels = {'app': name}
            spec = {'selector': {'matchLabels': labels}, 'template': {'metadata': {'labels': labels}, 'spec': {
                'terminationGracePeriodSeconds': 1,
                'hostNetwork': args.host_network,
                'containers': [{'name': 'logger', 'image': 'busybox:1.37',
                    'command': ['sh', '-c', 'echo hamn-kubernetes-e2e; sleep 3600'],
                    'resources': {'requests': {'cpu': '10m', 'memory': '8Mi'}, 'limits': {'cpu': '100m', 'memory': '32Mi'}}}]}}}
            if kind != 'DaemonSet':
                spec['replicas'] = 1
            if kind == 'StatefulSet':
                spec['serviceName'] = 'statefulset'
            items.append({'apiVersion': 'apps/v1', 'kind': kind, 'metadata': {'name': name, 'namespace': namespace}, 'spec': spec})
        run(kube + ['-n', namespace, 'create', '-f', '-'], json.dumps({'apiVersion': 'v1', 'kind': 'List', 'items': items}))
        for kind in ['deployment', 'statefulset', 'daemonset']:
            run(kube + ['-n', namespace, 'rollout', 'status', kind + '/' + kind, '--timeout=180s'], timeout=210)
        for resource in ['namespaces', 'nodes', 'pods', 'deployments', 'statefulsets', 'daemonsets', 'services', 'events', 'jobs', 'cronjobs', 'ingresses', 'pvcs']:
            assert isinstance(operation('k8s', resource, 'list'), list)
        for resource, name in [('deployments', 'deployment'), ('statefulsets', 'statefulset')]:
            for count in [3, 1]:
                operation('k8s', resource, 'scale', name, replicas=count, yes=True)
                detail = operation('k8s', resource, 'inspect', name)
                assert detail['object']['spec']['replicas'] == count, detail
        for resource, name in [('deployments', 'deployment'), ('statefulsets', 'statefulset'), ('daemonsets', 'daemonset')]:
            operation('k8s', resource, 'restart', name, yes=True)
            detail = operation('k8s', resource, 'inspect', name)
            assert detail['yaml'] and detail['object']['spec']['template']['metadata']['annotations']
            run(kube + ['-n', namespace, 'rollout', 'status', resource + '/' + name, '--timeout=180s'], timeout=210)
        pods = operation('k8s', 'pods', 'list')
        selected = next(pod for pod in pods if pod['metadata'].get('labels', {}).get('app') == 'deployment')
        name, uid = selected['metadata']['name'], selected['metadata']['uid']
        logs = run(hamn + ['k8s', 'pods', 'logs', name, '--container', 'logger', '--tail', '10'])
        records = [json.loads(line) for line in logs.splitlines()]
        assert any('hamn-kubernetes-e2e' in json.dumps(record) for record in records), records
        checks.append('k8s pods logs')
        operation('k8s', 'pods', 'delete', name, uid=uid, yes=True)
        rejected = subprocess.run(hamn + ['k8s', 'pods', 'delete', name, '--uid', uid + '-stale', '--yes'], text=True, capture_output=True, timeout=90)
        assert rejected.returncode and json.loads(rejected.stdout)['ok'] is False
        checks.append('stalePodRejected')
    finally:
        if created:
            run(kube + ['delete', 'namespace', namespace, '--wait=true', '--timeout=120s'], timeout=150)
        if any(path.read_bytes() != data for path, data in before.items()):
            raise RuntimeError('kubeconfig changed during validation')
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash, 'binary changed during validation'
    value = {'schemaVersion': 1, 'kind': 'hamn-external-kubernetes-e2e', 'passed': True,
        'binarySha256': binary_hash, 'context': args.context, 'namespace': namespace,
        'podNetwork': 'host' if args.host_network else 'cluster',
        'namespaceRemoved': True, 'kubeconfigUnchanged': True, 'checks': sorted(set(checks))}
    with args.output.open('x') as output:
        json.dump(value, output, sort_keys=True)
        output.write('\n')
    print('External Kubernetes lifecycle, logs, identity checks and cleanup: passed')


if __name__ == '__main__':
    main()
