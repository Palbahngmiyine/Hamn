#!/usr/bin/env python3
"""Actual kubectl endpoint overrides remain visible in list, PTY and confirmation."""
import http.server
import json
import os
import shutil
import subprocess
import sys
import threading
from test_tui_native_regressions import Harness
from test_tui_tls_target import Api as DiscoveryApi


def main():
    kubectl = shutil.which('kubectl')
    if not kubectl:
        print('SKIP: installed kubectl unavailable for cluster target comparison')
        return
    harness = Harness('kubernetes')
    servers, threads, requests = [], [], []

    class Api(DiscoveryApi):
        def do_GET(self):
            requests.append((self.server.label, self.path))
            if '/pods' not in self.path:
                return super().do_GET()
            pod = {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {
                'name': self.server.pod_name, 'namespace': 'test',
                'uid': 'fixture-uid', 'resourceVersion': '1'}}
            value = pod if '/pods/' in self.path else {
                'apiVersion': 'v1', 'kind': 'PodList', 'items': [pod]}
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    try:
        harness.until('old-target-row')
        root = harness.root
        for label in ('default', 'alternate'):
            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Api)
            server.label, server.pod_name = label, 'initial'
            servers.append(server)
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            threads.append(thread)
        config = root / 'kubeconfig'
        content = json.loads(config.read_text())
        content['clusters'][0]['cluster']['server'] = f'http://127.0.0.1:{servers[0].server_port}'
        content['clusters'].append({'name': 'alternate', 'cluster': {
            'server': f'http://127.0.0.1:{servers[1].server_port}'}})
        config.write_text(json.dumps(content))
        before = config.read_bytes()
        (root / 'bin/kubectl').write_text(f'#!{sys.executable}\nimport os, sys\n'
            + f'os.execv({kubectl!r}, [{kubectl!r}] + sys.argv[1:])\n')
        env = dict(os.environ, HOME=str(root), KUBECONFIG=str(config))
        for index, flag in enumerate(('--cluster alternate', '--cluster=alternate')):
            # Unique response names prevent a previous rendered query passing a new case.
            name = 'cluster-case-' + str(index)
            servers[1].pod_name = name
            direct = subprocess.run([kubectl, '--context', 'old-cluster', '--namespace',
                'test', 'get', 'pods'] + flag.split(), env=env, text=True,
                capture_output=True, timeout=10)
            assert direct.returncode == 0 and name in direct.stdout, direct
            assert requests[-1][0] == 'alternate', requests
            harness.send((':get pods ' + flag + '\r').encode(), name)
            assert flag in harness.screen.text(), harness.screen.text()
            assert requests[-1][0] == 'alternate', requests
            harness.send(b'd', 'Confirm delete')
            assert flag in harness.screen.text(), harness.screen.text()
            harness.send(b'n', '[Kubernetes]')  # Review confirmation without changing resources.
            harness.until(name)
            harness.send(b'\r', 'Exit code 0')
            assert flag in '\n'.join(harness.screen.text().splitlines()[:2]), harness.screen.text()
            assert requests[-1][0] == 'alternate' and '/pods/' + name in requests[-1][1], requests
            harness.send(b'\r', '[Kubernetes]')
        assert config.read_bytes() == before
        assert sorted(p.name for p in (root / '.hamn').iterdir()) == ['tui.json']
        print('PASS: both cluster override spellings select the alternate API and appear in list, confirmation and detail PTY; kubeconfig unchanged')
    finally:
        harness.close()
        for server in servers:
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=5)
            assert not thread.is_alive()


if __name__ == '__main__':
    main()
