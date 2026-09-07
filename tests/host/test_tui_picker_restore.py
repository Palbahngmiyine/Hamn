#!/usr/bin/env python3
"""Picker cancellation in real Hamn PTYs with disposable, recorded CLI peers."""
import os
import sys
import http.server
import json
import threading
from test_tui_native_regressions import Harness


PEER = r'''
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
root = Path(os.environ['FIXTURE_ROOT'])
with (root / 'calls').open('a') as output:
    output.write(json.dumps([Path(sys.argv[0]).name, args]) + '\n')
if 'context' in args:
    print(json.dumps({'Name':'external', 'DockerEndpoint':'unix:///external.sock'}))
elif 'ps' in args:
    for name in ['row-one', 'row-two', 'outside']:
        print(json.dumps({'ID':name, 'Names':name, 'State':'running'}))
elif 'get' in args:
    print(json.dumps({'items':[{'apiVersion':'v1', 'kind':'Pod', 'metadata':{
        'name':name, 'namespace':'chosen', 'resourceVersion':'1',
        'uid':'uid-' + name}} for name in ['row-one', 'row-two', 'outside']]}))
else:
    print('ACTION_DONE', flush=True)
'''


class NamespaceApi(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        assert self.path.startswith('/api/v1/namespaces'), self.path
        body = json.dumps({'apiVersion':'v1', 'kind':'NamespaceList', 'items':[
            {'apiVersion':'v1', 'kind':'Namespace', 'metadata':{'name':name}}
            for name in ('row-one', 'row-two')]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def prepare(harness):
    harness.until('old-target-row')
    for program in ('docker', 'kubectl'):
        replacement = harness.root / 'bin/replacement'
        replacement.write_text(f'#!{sys.executable}\n' + PEER)
        replacement.chmod(0o755)
        replacement.replace(harness.root / 'bin' / program)


def cancel_preserves_query(workspace, command, key, title, switch=False):
    harness = Harness(workspace)
    try:
        prepare(harness)
        before_config = (harness.root / 'kubeconfig').read_bytes()
        harness.send((':' + command + '\r').encode(), 'row-two')
        original = harness.calls()[-1]
        harness.send(b'/row\rj:SELECTION_BARRIER', ':SELECTION_BARRIER')
        os.write(harness.master, b'\x1b')
        harness.wait(lambda: ':SELECTION_BARRIER' not in harness.screen.text())
        harness.send(key, title)
        assert 'row-two' not in harness.screen.text() or key == b'n', harness.screen.text()
        if switch:
            other = '[Kubernetes]' if workspace == 'containers' else '[Containers]'
            harness.send(b'\t', other)
            harness.send(b'\t', title)
            # Reopening a selector also keeps the first return destination.
            harness.send(key + b':REOPEN_BARRIER', ':REOPEN_BARRIER')
            os.write(harness.master, b'\x1b')
            harness.wait(lambda: ':REOPEN_BARRIER' not in harness.screen.text())
        start = len(harness.calls())
        os.write(harness.master, b'\x1b')
        harness.wait(lambda: title not in harness.screen.text())
        harness.until('row-two')
        harness.wait(lambda: any(call == original for call in harness.calls()[start:]))
        harness.wait(lambda: '[loading]' not in harness.screen.text())
        assert title not in harness.screen.text(), harness.screen.text()
        assert 'outside' not in harness.screen.text(), harness.screen.text()
        # The filter and selected row must survive, including after the fresh query.
        harness.send(b'd', 'Confirm delete')
        assert 'row-two' in harness.screen.text(), harness.screen.text()
        assert 'explicit' in harness.screen.text(), harness.screen.text()
        assert not any('delete' in args or 'rm' in args for _, args in harness.calls())
        harness.send(b'n', '[Containers]' if workspace == 'containers' else '[Kubernetes]')
        assert (harness.root / 'kubeconfig').read_bytes() == before_config
    finally:
        harness.close()


def committed_navigation_is_not_undone(workspace, key, title, typed=False):
    harness = Harness(workspace)
    server = thread = None
    try:
        prepare(harness)
        if key == b'n':
            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), NamespaceApi)
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            config = harness.root / 'kubeconfig'
            content = json.loads(config.read_text())
            content['clusters'][0]['cluster']['server'] = f'http://127.0.0.1:{server.server_port}'
            config.write_text(json.dumps(content))
        original_config = (harness.root / 'kubeconfig').read_bytes()
        command = ('docker --context explicit ps --filter label=old' if workspace == 'containers'
                   else 'get pods --context explicit --namespace chosen -l app=old')
        if key == b'v': command = 'ps --filter label=old'
        harness.send((':' + command + '\r').encode(), 'row-two')
        harness.send(key, title)
        if typed:
            query = ('docker --context replacement ps --filter label=new' if workspace == 'containers'
                     else 'get pods --context replacement --namespace replacement -l app=new')
            harness.send((':' + query + '\r').encode(), '--context replacement')
        else:
            choice = 'external' if workspace == 'containers' else ('old-cluster' if key == b'e' else 'row-two')
            harness.send(('/' + choice + '\r').encode(), choice)
            harness.wait(lambda: '[loading]' not in harness.screen.text())
            harness.send(b'\r', 'docker containers' if workspace == 'containers' else 'kubectl pods')
        harness.until('outside')
        harness.wait(lambda: '[loading]' not in harness.screen.text())
        current = harness.calls()[-1]
        assert 'explicit' not in current[1], current
        assert not any(value in current[1] for value in ('label=old', 'app=old')), current
        harness.send(b'/outside\r', 'outside')
        harness.wait(lambda: 'row-one' not in harness.screen.text())
        harness.send(b'\x1b', 'row-one')
        assert 'outside' in harness.screen.text() and title not in harness.screen.text(), harness.screen.text()
        assert '--context explicit' not in harness.screen.text(), harness.screen.text()
        if typed: assert '--context replacement' in harness.screen.text(), harness.screen.text()
        assert (harness.root / 'kubeconfig').read_bytes() == original_config
    finally:
        harness.close()
        if server:
            server.shutdown(); server.server_close()
            thread.join(timeout=5)
            assert not thread.is_alive()


if __name__ == '__main__':
    for command in ('docker --host unix:///explicit.sock ps --filter label=app=x',
                    'docker --context explicit ps --filter label=app=x'):
        cancel_preserves_query('containers', command, b'e', 'Container environments', switch=True)
    for key, title in [(b'e', 'k8s contexts list'), (b'n', 'k8s namespaces list')]:
        cancel_preserves_query('kubernetes',
            'get pods --context explicit --namespace chosen -l app=x', key, title, switch=True)
    for workspace, key, title in [('containers', b'e', 'Container environments'),
                                  ('kubernetes', b'e', 'k8s contexts list'),
                                  ('kubernetes', b'n', 'k8s namespaces list')]:
        for typed in (False, True):
            committed_navigation_is_not_undone(workspace, key, title, typed)
    committed_navigation_is_not_undone('containers', b'v', 'vm list', typed=True)
    print('PASS: picker cancellation preserves query/filter/selection across workspace switches; actual choices and new queries commit navigation')
