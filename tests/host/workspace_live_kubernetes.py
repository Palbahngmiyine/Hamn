"""Disposable kind cluster in the owned Hamn Docker environment."""
import hashlib
import json
import subprocess
from physical_runtime import run
from workspace_live_terminal import Terminal, exercise


def assert_deployment_preserved(before, after):
    # Controllers can update status/metadata and resourceVersion between reads.
    # A rejected restart must preserve this selected object's desired state.
    assert after['metadata']['uid'] == before['metadata']['uid']
    assert after['spec'] == before['spec']
    assert after['metadata']['annotations']['guard-proof'] == before['metadata']['annotations']['guard-proof']


def guarded_mutations(root, runtime, config, env):
    """Prove menu preconditions against API replacement and concurrent changes."""
    name = 'hamn-guard-' + hashlib.sha256(str(root).encode()).hexdigest()[:12]
    evidence = {}
    owned = False

    def kubectl(*args):
        return run(['kubectl', '--kubeconfig', config, *args], env)

    def resource(kind, resource_name):
        args = ['get', kind, resource_name, '-o', 'json']
        if kind != 'namespace':
            args.extend(['--namespace', name])
        value = json.loads(kubectl(*args))
        evidence.setdefault('resourceSnapshots', []).append({'kind': kind, 'resource': value})
        (root / 'kubernetes-guarded-actions.json').write_text(json.dumps(evidence, indent=2))
        return value

    def create_namespace():
        nonlocal owned
        kubectl('create', 'namespace', name)
        owned = True
        kubectl('wait', '--for=jsonpath={.status.phase}=Active',
                'namespace/' + name, '--timeout=60s')
        return resource('namespace', name)

    def confirmed_action(kind, resource_name, key, action, change, expected_code):
        # Each terminal begins with no cached Kubernetes rows. Confirmation pauses
        # refresh while change() deterministically changes the server-side object.
        terminal = Terminal(runtime.binary, env, root)
        try:
            if not (runtime.home / '.hamn/tui.json').exists():
                terminal.until('Choose your default workspace')
                terminal.send(b'1\r')
            terminal.until('hamn-workspace-sentinel')
            query = f'kubectl get {kind} {resource_name}'
            if kind != 'namespaces':
                query += ' --namespace ' + name
            terminal.send((':' + query + '\r').encode(), '> ' + resource_name)
            terminal.send(key, f'Confirm {action} {kind} {resource_name}')
            change()
            terminal.send(b'y', 'Exit code ' + str(expected_code))
            if action == 'delete' and expected_code == 1:
                assert 'Conflict' in terminal.screen.text(), terminal.screen.text()
            terminal.send(b'\r')
        finally:
            # Esc dismisses an unfinished confirmation or returns from an exited
            # CLI, so an assertion failure does not leave the PTY in that view.
            terminal.send(b'\x1b')
            terminal.close()

    try:
        original = create_namespace()
        evidence['originalNamespaceUid'] = original['metadata']['uid']

        def replace_namespace():
            kubectl('delete', 'namespace', name, '--wait=true', '--timeout=60s')
            replacement = create_namespace()
            evidence['replacementNamespaceUid'] = replacement['metadata']['uid']
            assert evidence['replacementNamespaceUid'] != evidence['originalNamespaceUid']

        confirmed_action('namespaces', name, b'd', 'delete', replace_namespace, 1)
        assert resource('namespace', name)['metadata']['uid'] == evidence['replacementNamespaceUid']
        confirmed_action('namespaces', name, b'd', 'delete', lambda: None, 0)
        kubectl('wait', '--for=delete', 'namespace/' + name, '--timeout=60s')
        owned = False
        evidence['replacementPreservedThenFreshDeleteSucceeded'] = True

        create_namespace()
        deployment = {'apiVersion': 'apps/v1', 'kind': 'Deployment',
            'metadata': {'name': name, 'namespace': name}, 'spec': {'replicas': 0,
            'selector': {'matchLabels': {'app': name}}, 'template': {
            'metadata': {'labels': {'app': name}, 'annotations': {'keep': 'preserved'}},
            'spec': {'containers': [{'name': 'idle', 'image': 'busybox:1.37'}]}}}}
        manifest = root / 'guarded-deployment.json'
        manifest.write_text(json.dumps(deployment))

        def create_deployment():
            kubectl('create', '-f', str(manifest))
            kubectl('rollout', 'status', 'deployment/' + name,
                    '--namespace', name, '--timeout=60s')
            return resource('deployment', name)

        original_deployment = create_deployment()
        evidence['originalDeploymentUid'] = original_deployment['metadata']['uid']

        def replace_deployment():
            kubectl('delete', 'deployment', name, '--namespace', name,
                    '--wait=true', '--timeout=60s')
            replacement = create_deployment()
            evidence['replacementDeploymentUid'] = replacement['metadata']['uid']
            assert evidence['replacementDeploymentUid'] != evidence['originalDeploymentUid']

        confirmed_action('deployments', name, b'r', 'restart', replace_deployment, 1)
        replacement = resource('deployment', name)
        assert replacement['metadata']['uid'] == evidence['replacementDeploymentUid']
        assert 'kubectl.kubernetes.io/restartedAt' not in replacement['spec']['template']['metadata']['annotations']

        concurrent_expected = None
        def change_version():
            nonlocal concurrent_expected
            kubectl('annotate', 'deployment', name, '--namespace', name, 'guard-proof=changed')
            concurrent_expected = resource('deployment', name)
            evidence['concurrentVersion'] = concurrent_expected['metadata']['resourceVersion']

        confirmed_action('deployments', name, b'r', 'restart', change_version, 1)
        concurrent = resource('deployment', name)
        evidence['afterRejectedVersion'] = concurrent['metadata']['resourceVersion']
        assert_deployment_preserved(concurrent_expected, concurrent)
        assert 'kubectl.kubernetes.io/restartedAt' not in concurrent['spec']['template']['metadata']['annotations']
        confirmed_action('deployments', name, b'r', 'restart', lambda: None, 0)
        restarted = resource('deployment', name)
        assert restarted['metadata']['uid'] == evidence['replacementDeploymentUid']
        annotations = restarted['spec']['template']['metadata']['annotations']
        assert annotations['keep'] == 'preserved' and annotations['kubectl.kubernetes.io/restartedAt']
        evidence['replacementAndConcurrentVersionRejectedThenFreshRestartSucceeded'] = True
        (root / 'kubernetes-guarded-actions.json').write_text(json.dumps(evidence, indent=2))
        print('PASS: real Kubernetes guarded delete/restart reject stale UID/version and accept fresh selection', flush=True)
    finally:
        (root / 'kubernetes-guarded-actions.json').write_text(json.dumps(evidence, indent=2))
        if owned:
            kubectl('delete', 'namespace', name, '--ignore-not-found=true', '--wait=true', '--timeout=60s')


def kubernetes(root, runtime, create=True):
    config = root / 'kubeconfig'
    env = dict(runtime.environment, DOCKER_HOST='unix://' + str(runtime.home / '.hamn/verify/docker.sock'), KUBECONFIG=str(config))
    name = 'hamn-workspace-proof'
    def kubectl(*args): return run(['kubectl', '--kubeconfig', config, *args], env)
    try:
        if create:
            run(['kind', 'create', 'cluster', '--name', name, '--kubeconfig', config, '--wait', '120s'], env)
        kubectl('create', 'namespace', 'workspace-proof')
        kubectl('config', 'set-context', '--current', '--namespace=workspace-proof')
        pod = {'apiVersion':'v1','kind':'Pod','metadata':{'name':'workspace-http','namespace':'workspace-proof'},
               'spec':{'containers':[{'name':'http','image':'busybox:1.37','imagePullPolicy':'IfNotPresent',
                       'command':['sh','-c','mkdir -p /www; echo kube-http-proof > /www/index.html; exec httpd -f -p 8080 -h /www']}]}}
        manifest = root / 'pod.json'; manifest.write_text(json.dumps(pod))
        kubectl('apply', '-f', str(manifest))
        kubectl('wait', '-n', 'workspace-proof', '--for=condition=Ready', 'pod/workspace-http', '--timeout=120s')
        (root / 'configmap.json').write_text(json.dumps({'apiVersion':'v1','kind':'ConfigMap',
            'metadata':{'name':'tui-proof','namespace':'workspace-proof'},'data':{'proof':'preserved-cli'}}))
        before = config.read_bytes()
        exercise(root, runtime, config)
        assert config.read_bytes() == before, 'TUI selection changed kubeconfig'
        assert 'preserved-cli' in kubectl('get','configmap','tui-proof','-n','workspace-proof','-o','json')
        guarded_mutations(root, runtime, config, env)
        (root / 'kubernetes-version.json').write_text(kubectl('version','-o','json'))
        print('PASS: live Kubernetes query, apply, interactive exec, port-forward and unchanged kubeconfig', flush=True)
    finally:
        run(['kind','delete','cluster','--name',name,'--kubeconfig',config], env)
