#!/usr/bin/env python3
"""Picker cancellation in real Hamn PTYs with disposable, recorded CLI peers."""
import os
import sys
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
    for name in ['row-one', 'row-two']:
        print(json.dumps({'ID':name, 'Names':name, 'State':'running'}))
elif 'get' in args:
    print(json.dumps({'items':[{'apiVersion':'v1', 'kind':'Pod', 'metadata':{
        'name':name, 'namespace':'chosen', 'resourceVersion':'1',
        'uid':'uid-' + name}} for name in ['row-one', 'row-two']]}))
else:
    print('ACTION_DONE', flush=True)
'''


def cancel_preserves_query(workspace, command, key, title):
    harness = Harness(workspace)
    try:
        harness.until('old-target-row')
        for program in ('docker', 'kubectl'):
            replacement = harness.root / 'bin/replacement'
            replacement.write_text(f'#!{sys.executable}\n' + PEER)
            replacement.chmod(0o755)
            replacement.replace(harness.root / 'bin' / program)
        before_config = (harness.root / 'kubeconfig').read_bytes()
        harness.send((':' + command + '\r').encode(), 'row-two')
        original = harness.calls()[-1]
        harness.send(b'/row\rj:SELECTION_BARRIER', ':SELECTION_BARRIER')
        os.write(harness.master, b'\x1b')
        harness.wait(lambda: ':SELECTION_BARRIER' not in harness.screen.text())
        harness.send(key, title)
        assert 'row-two' not in harness.screen.text() or key == b'n', harness.screen.text()
        start = len(harness.calls())
        os.write(harness.master, b'\x1b')
        harness.wait(lambda: title not in harness.screen.text())
        harness.until('row-two')
        harness.wait(lambda: any(call == original for call in harness.calls()[start:]))
        harness.wait(lambda: '[loading]' not in harness.screen.text())
        assert title not in harness.screen.text(), harness.screen.text()
        # The filter and selected row must survive, including after the fresh query.
        harness.send(b'd', 'Confirm delete')
        assert 'row-two' in harness.screen.text(), harness.screen.text()
        assert 'explicit' in harness.screen.text(), harness.screen.text()
        assert not any('delete' in args or 'rm' in args for _, args in harness.calls())
        harness.send(b'n', '[Containers]' if workspace == 'containers' else '[Kubernetes]')
        assert (harness.root / 'kubeconfig').read_bytes() == before_config
    finally:
        harness.close()


if __name__ == '__main__':
    for command in ('docker --host unix:///explicit.sock ps --filter label=app=x',
                    'docker --context explicit ps --filter label=app=x'):
        cancel_preserves_query('containers', command, b'e', 'Container environments')
    for key, title in [(b'e', 'k8s contexts list'), (b'n', 'k8s namespaces list')]:
        cancel_preserves_query('kubernetes',
            'get pods --context explicit --namespace chosen -l app=x', key, title)
    print('PASS: Docker and Kubernetes picker cancellation preserves query target and selected resource')
