"""Review UX acceptance against an existing, workspace-owned kind cluster.

No VM or cluster is started here. Each run owns a UUID namespace and CRD, checks
API UIDs before cleanup, and preserves the caller's kubeconfig bytes. Actual PTY,
kubectl API results, process identities and HTTP bodies are independent evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys
import time
import urllib.request
import uuid

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / 'packaging/release'))
from physical_runtime import run
from workspace_live_terminal import Terminal

OWNER = 'io.hamn.review-owner'
BROWSER, SESSIONS = b'\x1b\x02', b'\x1b\x13'


def assert_identity(value, uid, token):
    assert value['metadata']['uid'] == uid, 'resource was replaced; refusing cleanup'
    assert value['metadata'].get('labels', {}).get(OWNER) == token, 'resource ownership changed'


def assert_relations(pods, events, pod, decoy, current_event, stale_event):
    pod_ids = {item['metadata']['uid'] for item in pods['items']}
    event_ids = {item['metadata']['uid'] for item in events['items']}
    assert pod_ids == {pod['metadata']['uid']} and decoy['metadata']['uid'] not in pod_ids
    assert current_event['metadata']['uid'] in event_ids
    assert stale_event['metadata']['uid'] not in event_ids
    assert all(item['involvedObject']['uid'] == pod['metadata']['uid'] for item in events['items'])


def processes():
    rows = {}
    for line in subprocess.check_output(['/bin/ps', '-axo', 'pid=,ppid=,pgid=,lstart=,args='], text=True).splitlines():
        parts = line.split(None, 8)
        if len(parts) == 9:
            pid, parent, group = map(int, parts[:3])
            rows[pid] = {'pid': pid, 'parent': parent, 'group': group,
                         'started': ' '.join(parts[3:8]), 'command': parts[8]}
    return rows


def same_process(expected, observed):
    return observed is not None and all(expected[key] == observed[key] for key in ('pid', 'group', 'started', 'command'))


def wait_gone(owned, timeout=10):
    deadline = time.monotonic() + timeout
    while True:
        current = processes()
        alive = [p for p in owned if same_process(p, current.get(p['pid']))]
        if not alive:
            return
        assert time.monotonic() < deadline, ('owned CLI survived cleanup', alive)
        select.select([], [], [], 0.05)


def cleanup_processes(owned):
    # Used only after assertions/normal TUI cleanup. Never signal by command name
    # or an unverified recycled PID; each entry was a direct PTY child we owned.
    current = processes()
    for process in owned:
        if same_process(process, current.get(process['pid'])) and process['pid'] == process['group']:
            os.killpg(process['group'], signal.SIGTERM)
    wait_gone(owned)


def assert_port_closed(port):
    with socket.socket() as probe:
        probe.settimeout(1)
        assert probe.connect_ex(('127.0.0.1', port)) != 0, f'forward listener survived: {port}'


def choose(terminal, title, option):
    terminal.until(title)
    rows = [line.strip(' │') for line in terminal.screen.text().splitlines()]
    options = [line.removeprefix('> ').strip() for line in rows[1:-1] if line.strip()]
    assert option in options, (option, options)
    terminal.send(b'\x1b[B' * options.index(option) + b'\r')


def query(terminal, kind, name, namespace):
    suffix = ' --namespace ' + namespace if namespace else ''
    terminal.send(f':kubectl get {kind} {name}{suffix}\r'.encode())
    terminal.wait(lambda: f'kubectl {kind}' in terminal.screen.text()
                  and '> ' + name in terminal.screen.text()
                  and '[loading]' not in terminal.screen.text())


def management_review(root, runtime, config, env, cluster_name='hamn-workspace-proof'):
    root, config = Path(root).resolve(), Path(config).resolve()
    owner = json.loads((root / 'ownership.json').read_text())
    assert owner['profile'] == 'verify' and Path(owner['home']).resolve() == runtime.home.resolve()
    assert runtime.home.resolve() == root / 'home' and config.parent == root
    assert env['DOCKER_HOST'] == 'unix://' + str(runtime.home / '.hamn/verify/docker.sock')
    before_config = config.read_bytes()
    context = 'kind-' + cluster_name
    cluster = json.loads(runtime.engine('inspect', cluster_name + '-control-plane', profile='verify'))[0]
    assert cluster['Config']['Labels']['io.x-k8s.kind.cluster'] == cluster_name
    token = uuid.uuid4().hex
    namespace, group = 'review-' + token[:10], 'r' + token[:10] + '.hamn.test'
    crd_name = 'probes.' + group
    label = {OWNER: token}
    owned, children, ports, terminal = [], [], [], None
    evidence = {'runId': token, 'cluster': cluster_name, 'context': context,
                'namespace': namespace, 'candidateSHA256': hashlib.sha256(runtime.binary.read_bytes()).hexdigest(),
                'kubeconfigSHA256': hashlib.sha256(before_config).hexdigest(), 'resources': {}}
    evidence_path = root / ('management-review-' + token + '.json')

    def kubectl(*args):
        return run(['kubectl', '--kubeconfig', config, '--context', context, *args], env, timeout=150)

    def get(kind, name, ns=None):
        flags = ['--namespace', ns] if ns else []
        return json.loads(kubectl('get', kind, name, *flags, '-o', 'json'))

    def create(value):
        result = json.loads(run(['kubectl', '--kubeconfig', config, '--context', context,
                                 'create', '-f', '-', '-o', 'json'], env, timeout=150, data=json.dumps(value)))
        evidence['resources'][result['kind'] + '/' + result['metadata']['name']] = result
        return result

    def http(port):
        direct = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with direct.open(f'http://127.0.0.1:{port}', timeout=10) as response:
            body = response.read(4096)
        assert body == (token + '\n').encode(), (port, body)
        return body.decode()

    try:
        assert kubectl('config', 'current-context').strip() == context
        owned.append(('namespace', namespace, None))
        ns = create({'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': namespace, 'labels': label}})
        owned.pop()
        owned.append(('namespace', namespace, ns['metadata']['uid']))
        owned.append(('customresourcedefinition', crd_name, None))
        crd = create({'apiVersion': 'apiextensions.k8s.io/v1', 'kind': 'CustomResourceDefinition',
            'metadata': {'name': crd_name, 'labels': label}, 'spec': {'group': group, 'scope': 'Namespaced',
            'names': {'plural': 'probes', 'singular': 'probe', 'kind': 'ReviewProbe'},
            'versions': [{'name': 'v1', 'served': True, 'storage': True, 'schema': {'openAPIV3Schema': {
                'type': 'object', 'properties': {'spec': {'type': 'object', 'properties': {'message': {'type': 'string'}}}}}}}]}})
        owned.pop()
        owned.append(('customresourcedefinition', crd_name, crd['metadata']['uid']))
        kubectl('wait', '--for=condition=Established', 'crd/' + crd_name, '--timeout=60s')
        custom = create({'apiVersion': group + '/v1', 'kind': 'ReviewProbe',
            'metadata': {'name': 'read-only', 'namespace': namespace, 'labels': label}, 'spec': {'message': token}})
        command = f'mkdir -p /www; echo {token} > /www/index.html; echo LOG-{token}; exec httpd -f -p 8080 -h /www'
        container = {'name': 'web', 'image': 'busybox:1.37', 'imagePullPolicy': 'IfNotPresent',
                     'command': ['sh', '-c', command]}
        deployment = create({'apiVersion': 'apps/v1', 'kind': 'Deployment', 'metadata': {
            'name': 'web', 'namespace': namespace, 'labels': label}, 'spec': {'replicas': 1,
            'selector': {'matchLabels': {'review': token}, 'matchExpressions': [{'key': 'tier', 'operator': 'In', 'values': ['frontend']}]},
            'template': {'metadata': {'labels': {'review': token, 'tier': 'frontend'}}, 'spec': {'containers': [container]}}}})
        decoy = create({'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {'name': 'decoy', 'namespace': namespace,
            'labels': {'review': token, 'tier': 'backend'}}, 'spec': {'containers': [container]}})
        kubectl('rollout', 'status', 'deployment/web', '--namespace', namespace, '--timeout=120s')
        selector = 'review=' + token + ',tier in (frontend)'
        pods = json.loads(kubectl('get', 'pods', '--namespace', namespace, '--selector', selector, '-o', 'json'))
        assert len(pods['items']) == 1
        pod = pods['items'][0]
        pod_name, pod_uid = pod['metadata']['name'], pod['metadata']['uid']
        kubectl('wait', '--for=condition=Ready', 'pod/' + pod_name, '--namespace', namespace, '--timeout=120s')
        replica = get('replicaset', pod['metadata']['ownerReferences'][0]['name'], namespace)
        assert replica['metadata']['ownerReferences'][0]['uid'] == deployment['metadata']['uid']
        current_event = stale_event = None
        for name, uid in [('selected-event', pod_uid), ('stale-event', str(uuid.uuid4()))]:
            event = create({'apiVersion': 'v1', 'kind': 'Event', 'metadata': {'name': name, 'namespace': namespace},
                'involvedObject': {'apiVersion': 'v1', 'kind': 'Pod', 'name': pod_name, 'namespace': namespace, 'uid': uid},
                'reason': 'ReviewSelected' if uid == pod_uid else 'ReviewStale', 'message': token, 'type': 'Normal'})
            if uid == pod_uid: current_event = event
            else: stale_event = event
        events = json.loads(kubectl('get', 'events', '--namespace', namespace, '--field-selector', 'involvedObject.uid=' + pod_uid, '-o', 'json'))
        assert_relations(pods, events, pod, decoy, current_event, stale_event)
        evidence['independentRelations'] = {'pods': pods, 'events': events, 'replicaSet': replica}
        assert 'LOG-' + token in kubectl('logs', pod_name, '--namespace', namespace, '--container', 'web', '--tail=200')

        terminal = Terminal(runtime.binary, dict(env, KUBECONFIG=str(config)), root)
        if not (runtime.home / '.hamn/tui.json').exists():
            terminal.until('Choose your default workspace'); terminal.send(b'1\r')
        terminal.until(': command')
        # The deliberately unrelated pod sorts first in a namespace-wide list.
        # Select the intended starting object explicitly; relationship queries
        # below must still prove that their selectors exclude the decoy.
        query(terminal, 'pods', pod_name, namespace)
        for kind, name, ns_value, expected in [('crds', crd_name, None, crd), ('probes.' + group, 'read-only', namespace, custom)]:
            query(terminal, kind, name, ns_value)
            terminal.send(b'm', 'Resource actions')
            menu = terminal.screen.text()
            assert 'inspect' in menu and all(action not in menu for action in ('delete', 'restart', 'logs', 'related-'))
            terminal.send(b'\r', 'Exit code 0')
            if expected['metadata']['uid'] not in terminal.screen.text():
                terminal.send(b'\x1b[5~', expected['metadata']['uid'])
            terminal.send(b'\r')
            query(terminal, kind, name, ns_value)
            terminal.send(b'd:READ_ONLY_BARRIER', ':READ_ONLY_BARRIER')
            assert 'Confirm delete' not in terminal.screen.text()
            terminal.send(b'\x1b')
            observed = get(kind, name, ns_value)
            assert observed['metadata']['uid'] == expected['metadata']['uid'] and observed['spec'] == expected['spec']
        query(terminal, 'deployments', 'web', namespace)
        terminal.send(b'm', 'related-pods'); choose(terminal, 'Resource actions', 'related-pods')
        terminal.until('> ' + pod_name)
        terminal.wait(lambda: '[loading]' not in terminal.screen.text())
        assert 'decoy' not in terminal.screen.text()
        evidence['relatedPodsScreen'] = terminal.screen.text()
        terminal.send(b'm', 'related-events'); choose(terminal, 'Resource actions', 'related-events')
        terminal.until('selected-event'); terminal.wait(lambda: '[loading]' not in terminal.screen.text())
        assert 'stale-event' not in terminal.screen.text()
        evidence['relatedEventsScreen'] = terminal.screen.text()
        query(terminal, 'pods', pod_name, namespace)
        terminal.send(b'l', 'Logs (Enter selects'); choose(terminal, 'Logs (Enter selects', 'web: follow latest 200 lines')
        terminal.until('LOG-' + token); terminal.send(BROWSER, '> ' + pod_name)
        for _ in range(2):
            listener = socket.socket(); listener.bind(('127.0.0.1', 0)); ports.append(listener)
        port_numbers = [listener.getsockname()[1] for listener in ports]
        for listener, port in zip(ports, port_numbers):
            listener.close()
            terminal.send(f':kubectl port-forward --namespace {namespace} pod/{pod_name} {port}:8080\r'.encode(),
                          f'Forwarding from 127.0.0.1:{port}')
            http(port)
            terminal.send(BROWSER, '> ' + pod_name)
        children = [p for p in processes().values() if p['parent'] == terminal.child.pid
                    and namespace in p['command'] and (' logs ' in p['command'] or ' port-forward ' in p['command'])]
        assert len(children) == 3 and all(p['group'] == p['pid'] for p in children), children
        evidence['ownedCliProcesses'] = children
        terminal.send(SESSIONS, 'Sessions: Enter resumes')
        for label in [f'logs pods/{pod_name} container=web', *[f'port-forward pod/{pod_name} {port}:8080' for port in port_numbers]]:
            terminal.until(label)
        evidence['sessionsScreen'] = terminal.screen.text()
        evidence['httpWhileDetached'] = {str(port): http(port) for port in port_numbers}
        # The last detached session is selected; closing it must spare both the
        # first forward and the independent, still-running log stream.
        terminal.send(b'd')
        removed = [p for p in children if str(port_numbers[1]) + ':8080' in p['command']]
        assert len(removed) == 1
        wait_gone(removed); assert_port_closed(port_numbers[1]); http(port_numbers[0])
        current = processes()
        assert all(same_process(p, current.get(p['pid'])) for p in children if p not in removed)
        terminal.send(b'\x1b', '> ' + pod_name)
        terminal.close(); terminal = None
        wait_gone(children)
        for port in port_numbers: assert_port_closed(port)
        assert config.read_bytes() == before_config
        evidence['passed'] = True
        print('PASS: live CRD/CR read-only, selector/UID navigation, concurrent logs/two forwards and owned cleanup', flush=True)
    finally:
        cleanup_errors = []
        if terminal:
            children.extend(p for p in processes().values() if p['parent'] == terminal.child.pid
                            and namespace in p['command'] and p['group'] == p['pid'] and p not in children)
            try: terminal.close()
            except Exception as error: cleanup_errors.append(str(error))
        try: cleanup_processes(children)
        except Exception as error: cleanup_errors.append(str(error))
        for listener in ports: listener.close()
        for kind, name, uid in reversed(owned):
            try:
                value = kubectl('get', kind, name, '--ignore-not-found', '-o', 'json')
                if not value.strip(): continue
                value = json.loads(value)
                # A timed-out create can have succeeded. Adopt only the exact
                # random ownership label and record its observed API UID.
                assert_identity(value, uid or value['metadata']['uid'], token)
                prefix = '/api/v1/namespaces/' if kind == 'namespace' else '/apis/apiextensions.k8s.io/v1/customresourcedefinitions/'
                body = {'apiVersion': 'v1', 'kind': 'DeleteOptions',
                        'preconditions': {'uid': value['metadata']['uid']}}
                run(['kubectl', '--kubeconfig', config, '--context', context, 'delete',
                     '--raw', prefix + name, '--filename', '-'], env, timeout=90, data=json.dumps(body))
                kubectl('wait', '--for=delete', kind + '/' + name, '--timeout=90s')
            except Exception as error: cleanup_errors.append(str(error))
        if config.read_bytes() != before_config: cleanup_errors.append('source kubeconfig changed')
        evidence['cleanupErrors'] = cleanup_errors
        evidence_path.write_text(json.dumps(evidence, indent=2))
        assert not cleanup_errors, (evidence_path, cleanup_errors)
    return evidence_path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True, help='existing test_workspace_live.py ownership root')
    parser.add_argument('--binary', type=Path, default=REPO / 'build/hamn')
    parser.add_argument('--cluster', default='hamn-workspace-proof')
    args = parser.parse_args()
    from test_workspace_live import prepare
    root, runtime = prepare(args.binary.resolve(), None, args.root)
    config = root / 'kubeconfig'
    env = dict(runtime.environment, DOCKER_HOST='unix://' + str(runtime.home / '.hamn/verify/docker.sock'), KUBECONFIG=str(config))
    print(management_review(root, runtime, config, env, args.cluster))


if __name__ == '__main__':
    main()
