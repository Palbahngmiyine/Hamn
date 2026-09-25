#!/usr/bin/env python3
"""Identity refresh, pasted filters, diagnostics and bounded log menus via a real PTY."""
import json
import os
import sys
from test_tui_native_regressions import Harness

PEER = '''import json, os, sys
from pathlib import Path
root = Path(os.environ['FIXTURE_ROOT']); args = sys.argv[1:]
with (root/'calls').open('a') as out: out.write(json.dumps([Path(sys.argv[0]).name,args])+'\\n')
if 'get' in args or 'ps' in args:
 rows=json.loads((root/'rows.json').read_text())
 if 'get' in args: print(json.dumps({'items':rows}))
 else:
  for row in rows: print(json.dumps(row))
elif 'logs' in args: print('LOG_OPTIONS_ACCEPTED', flush=True)
else: print('ACTION_DONE', flush=True)
'''


def pod(name, uid, namespace='work'):
    return {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {'name': name, 'uid': uid,
        'namespace': namespace, 'resourceVersion': '1'}, 'spec': {'containers': [{'name': 'app'}]},
        'status': {'phase': 'Running', 'containerStatuses': [{'name': 'app', 'ready': False,
            'restartCount': 42, 'state': {'waiting': {'reason': 'CrashLoopBackOff'}}}]}}


def write_rows(harness, rows):
    stage = harness.root / 'rows.tmp'
    stage.write_text(json.dumps(rows)); stage.replace(harness.root / 'rows.json')


def scenario(workspace):
    harness = Harness(workspace)
    try:
        harness.until('old-target-row')
        kubernetes = workspace == 'kubernetes'
        def row(name, uid, namespace='work'):
            return pod(name, uid, namespace) if kubernetes else {'ID': uid, 'Names': name, 'State': 'running'}
        alpha, beta, gamma = row('alpha', 'id-a'), row('beta', 'id-b', 'west'), row('gamma', 'id-c')
        write_rows(harness, [alpha, beta, gamma])
        peer = harness.root / 'bin' / ('kubectl' if kubernetes else 'docker')
        peer.write_text(f'#!{sys.executable}\n' + PEER)
        harness.send(b':get pods -A\r' if kubernetes else b':ps\r', 'beta')
        if kubernetes:
            harness.until('CrashLoopBackOff'); harness.until('42')
        os.write(harness.master, b'j')
        # A changed marker proves the reordered response has actually rendered.
        write_rows(harness, [row('marker-reordered', 'id-d'), gamma, alpha, beta])
        harness.send(b'R', 'marker-reordered')
        expected = 'Confirm delete pods beta' if kubernetes else 'Confirm delete containers id-b'
        harness.send(b'd', expected)
        if kubernetes: assert 'selected namespace: west' in harness.screen.text()
        harness.send(b'n', 'marker-reordered')
        write_rows(harness, [row('marker-removed', 'id-d'), alpha, gamma])
        harness.send(b'R', 'marker-removed')
        os.write(harness.master, b'd:SELECTION_BARRIER')
        harness.until(':SELECTION_BARRIER')
        assert 'Confirm delete' not in harness.screen.text()
        os.write(harness.master, b'\x1b')
        # An ESC and R read together are one Alt-R key, so the refresh must
        # wait until the command line has closed.
        harness.wait(lambda: ':SELECTION_BARRIER' not in harness.screen.text())
        write_rows(harness, [alpha, beta, row('marker-paste', 'id-d')])
        harness.send(b'R', 'marker-paste')
        harness.send(b'/\x1b[200~beta\x1b[201~\r', 'beta')
        os.write(harness.master, b'd:FILTER_BARRIER')
        harness.until(':FILTER_BARRIER')
        assert 'Confirm delete' not in harness.screen.text()
        os.write(harness.master, b'\x1b')
        harness.wait(lambda: ':FILTER_BARRIER' not in harness.screen.text())
        os.write(harness.master, b'j')
        harness.send(b'd', expected)
        harness.send(b'n', 'beta')
        harness.send(b'l', 'Follow latest 200 lines')
        if kubernetes: os.write(harness.master, b'\x1b[B\x1b[B\x1b[B')
        harness.send(b'\r', 'LOG_OPTIONS_ACCEPTED')
        harness.until('Exit code 0')
        commands = [args for _, args in harness.calls() if 'logs' in args]
        assert len(commands) == 1, commands
        args = commands[0]
        assert '--tail=200' in args and '--timestamps' in args, args
        if kubernetes:
            assert '--previous' in args and '--follow' not in args, args
            assert args[args.index('--container') + 1] == 'app', args
        else: assert '--follow' in args and args[-1] == 'id-b', args
        harness.send(b'\r', 'beta')
        harness.wait(lambda: '[loading]' not in harness.screen.text())
        if kubernetes:
            harness.send(b'm', 'related-events')
            harness.send(b'\x1b[B\x1b[B\x1b[B\x1b[B\r', 'kubectl events')
            harness.wait(lambda: '[loading]' not in harness.screen.text())
            related = [args for _, args in harness.calls() if 'events' in args]
            assert related[-1][related[-1].index('get'):related[-1].index('get')+4] == ['get', 'events', '--field-selector', 'involvedObject.uid=id-b'], related
            assert related[-1][related[-1].index('--namespace') + 1] == 'west', related
        harness.send(b'f', 'Favorite toggled')
        saved = json.loads((harness.root / '.hamn/tui.json').read_text())
        assert len(saved['favorites']) == 1
        harness.send(b'F', 'Favorite targets')
        harness.send(b'\r', 'beta')
        assert not any('delete' in args or 'rm' in args for _, args in harness.calls())
    finally:
        harness.close()


def compose_navigation():
    harness = Harness('containers')
    try:
        harness.until('old-target-row')
        write_rows(harness, [{'ID': 'service-id', 'Names': 'project-service', 'State': 'running'}])
        peer = harness.root / 'bin/docker'
        source = PEER.replace("if 'get' in args or 'ps' in args:", "if 'compose' in args:\n print(json.dumps([{'Name':'review-project','Status':'running(1)','ConfigFiles':'compose.yml'}]))\nelif 'get' in args or 'ps' in args:")
        peer.write_text(f'#!{sys.executable}\n' + source)
        harness.send(b':compose ls\r', 'review-project')
        harness.send(b'm', 'Resource actions')
        assert 'Project containers' in harness.screen.text()
        assert 'related-pods' not in harness.screen.text() and 'inspect' not in harness.screen.text()
        harness.send(b'\x1b', 'review-project')
        harness.send(b'\r', 'project-service')
        queries = [args for _, args in harness.calls() if 'ps' in args and '--filter' in args]
        assert queries[-1][queries[-1].index('--filter') + 1] == 'label=com.docker.compose.project=review-project', queries
        assert not any('down' in args for _, args in harness.calls())
    finally:
        harness.close()


if __name__ == '__main__':
    for workspace in ('containers', 'kubernetes'): scenario(workspace)
    compose_navigation()
    print('PASS: selection identity, removal, pasted filters, diagnostics, log options, relationships, Compose and favorites')
