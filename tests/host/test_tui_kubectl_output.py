#!/usr/bin/env python3
"""Installed kubectl keeps grouped output and live watch semantics in the TUI."""
import http.server
import json
import os
import select
import shutil
import signal
import subprocess
import threading
import time
import urllib.parse
from test_tui_native_regressions import Harness
from test_tui_tls_target import Api as DiscoveryApi


def main():
    kubectl = shutil.which('kubectl')
    if not kubectl:
        print('SKIP: installed kubectl unavailable; grouped options are covered in Rust')
        return
    harness = Harness('kubernetes', namespace='ui-ns')
    requests, release = [], threading.Event()
    server = thread = direct_watch = alternate = alternate_thread = None

    def pod(name, namespace='test'):
        return {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {'name': name,
            'namespace': namespace, 'uid': 'fixture-uid', 'resourceVersion': '1'},
            'status': {'phase': 'Running'}}

    class Api(DiscoveryApi):
        def do_GET(self):
            url = urllib.parse.urlsplit(self.path)
            if '/pods' not in url.path:
                return super().do_GET()
            label = getattr(self.server, 'fixture_label', 'format')
            namespace = url.path.split('/namespaces/')[1].split('/')[0] if '/namespaces/' in url.path else 'test'
            requests.append((url.path, urllib.parse.parse_qs(url.query), label))
            watching = requests[-1][1].get('watch') in (['true'], ['1'])
            value = {'type': 'ADDED', 'object': pod('live-watch-row')} if watching else {
                'apiVersion': 'v1', 'kind': 'PodList', 'metadata': {'resourceVersion': '1'},
                'items': [pod(label + '-fixture', namespace)]}
            if '/pods/' in url.path:
                value = pod(label + '-fixture', namespace)
            data = json.dumps(value).encode() + b'\n'
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            if not watching:
                self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            self.wfile.flush()
            if watching:
                release.wait(30)

    try:
        harness.until('old-target-row')
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Api)
        server.daemon_threads = False
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        config = harness.root / 'kubeconfig'
        content = json.loads(config.read_text())
        content['clusters'][0]['cluster']['server'] = f'http://127.0.0.1:{server.server_port}'
        config.write_text(json.dumps(content))
        before = config.read_bytes()
        wrapper = harness.root / 'bin/kubectl'
        wrapper.write_text(f'#!{os.sys.executable}\nimport os, sys\nos.execv({kubectl!r}, [{kubectl!r}] + sys.argv[1:])\n')
        env = dict(os.environ, HOME=str(harness.root), KUBECONFIG=str(config))
        for index, (flags, all_namespaces) in enumerate([
            ('--all-namespaces=false', False), ('-A=false', False),
            ('-A --all-namespaces=false', False), ('-AA=false', False),
            ('--all-namespaces=false -A', True), ('-A=0', False),
            ('-A=False', False), ('-A=TRUE', True),
        ]):
            server.fixture_label = f'scope-{index}'
            direct = subprocess.run([kubectl, '--context', 'old-cluster', '--namespace',
                'ui-ns', 'get', 'pods'] + flags.split(), env=env, capture_output=True,
                text=True, timeout=10)
            assert direct.returncode == 0, direct.stderr
            expected = '/api/v1/pods' if all_namespaces else '/api/v1/namespaces/ui-ns/pods'
            assert requests[-1][0] == expected, requests
            harness.send(b':get pods ' + flags.encode() + b'\r', f'scope-{index}-fixture')
            assert requests[-1][0] == expected, requests
            assert ('all namespaces' in harness.screen.text()) == all_namespaces, harness.screen.text()
        server.fixture_label = 'format'
        harness.send(b':get pods\r', 'format-fixture')
        for flags in (['-Aoyaml'], ['-Ao', 'yaml'], ['-Aojson']):
            args = ['get', 'pods'] + flags
            direct = subprocess.run([kubectl] + args, env=env, capture_output=True,
                text=True, timeout=10)
            assert direct.returncode == 0, direct.stderr
            harness.send(b':' + ' '.join(args).encode() + b'\r', 'Exit code 0')
            screen = harness.screen.text()
            # Complete line checks distinguish YAML from JSON and preserve values.
            for line in direct.stdout.splitlines():
                assert line.strip() in screen, (flags, line, screen)
            assert requests[-1][0] == '/api/v1/pods', requests
            harness.send(b'\r', '[Kubernetes]')
            harness.until('format-fixture')

        alternate = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Api)
        alternate.fixture_label = 'explicit'
        alternate_thread = threading.Thread(target=alternate.serve_forever)
        alternate_thread.start()
        endpoint = f'http://127.0.0.1:{alternate.server_port}'
        for flag in ('-As' + endpoint, '-As ' + endpoint, '-As=' + endpoint):
            direct = subprocess.run([kubectl, 'get', 'pods'] + flag.split(), env=env,
                capture_output=True, text=True, timeout=10)
            assert direct.returncode == 0 and 'explicit-fixture' in direct.stdout, direct
            harness.send(b':get pods ' + flag.encode() + b'\r', 'explicit-fixture')
            header = harness.screen.text()
            harness.send(b'\r', 'Exit code 0')
            assert 'name: explicit-fixture' in harness.screen.text(), harness.screen.text()
            assert requests[-1][0].endswith('/pods/explicit-fixture') and requests[-1][2] == 'explicit', requests
            assert endpoint in header, header
            harness.send(b'\r', '[Kubernetes]')
            harness.until('explicit-fixture')
            harness.send(b':get pods\r', 'format-fixture')

        direct_watch = subprocess.Popen([kubectl, 'get', 'pods', '-Aw'], env=env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        output = b''
        deadline = time.monotonic() + 10
        while b'live-watch-row' not in output:
            remaining = deadline - time.monotonic()
            assert remaining > 0 and select.select([direct_watch.stdout], [], [], remaining)[0], output
            data = os.read(direct_watch.stdout.fileno(), 65536)
            assert data, output
            output += data
        assert direct_watch.poll() is None, 'watch must display events before exit'
        direct_watch.send_signal(signal.SIGINT)
        direct_watch.communicate(timeout=5)
        code = direct_watch.returncode
        expected = code if code >= 0 else 128 - code
        harness.send(b':get pods -Aw\r', 'live-watch-row')
        assert 'Exit code' not in harness.screen.text(), harness.screen.text()
        assert requests[-1][0] == '/api/v1/pods', requests
        assert requests[-1][1].get('watch') in (['true'], ['1']), requests
        harness.send(b'\x03', f'Exit code {expected}')
        harness.send(b'\r', '[Kubernetes]')
        harness.until('format-fixture')
        assert config.read_bytes() == before, 'query changed kubeconfig'
        assert sorted(p.name for p in (harness.root / '.hamn').iterdir()) == ['tui.json']
        print('PASS: grouped kubectl output/watch and explicit server retained by selected detail; SIGINT and restoration')
    finally:
        if direct_watch and direct_watch.poll() is None:
            direct_watch.kill()
            direct_watch.communicate(timeout=5)
        release.set()
        harness.close()
        if server:
            server.shutdown()
            server.server_close()
        if thread:
            thread.join(timeout=5)
            assert not thread.is_alive()
        if alternate:
            alternate.shutdown()
            alternate.server_close()
        if alternate_thread:
            alternate_thread.join(timeout=5)
            assert not alternate_thread.is_alive()


if __name__ == '__main__':
    main()
