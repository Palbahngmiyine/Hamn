"""Disposable kind cluster in the owned Hamn Docker environment."""
import json
import subprocess
from physical_runtime import run
from workspace_live_terminal import exercise


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
        (root / 'kubernetes-version.json').write_text(kubectl('version','-o','json'))
        print('PASS: live Kubernetes query, apply, interactive exec, port-forward and unchanged kubeconfig', flush=True)
    finally:
        run(['kind','delete','cluster','--name',name,'--kubeconfig',config], env)
