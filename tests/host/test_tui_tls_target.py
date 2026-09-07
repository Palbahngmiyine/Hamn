#!/usr/bin/env python3
"""Use installed kubectl and a disposable HTTPS API to verify menu TLS targeting."""
import http.server
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import threading
from test_tui_native_regressions import Harness


class Api(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        pod = {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {
            'name': 'tls-pod', 'namespace': 'test', 'uid': 'uid-1', 'resourceVersion': '1'}}
        if self.path.startswith('/api/v1/namespaces/test/pods/'):
            body = pod
        elif self.path.startswith('/api/v1/namespaces/test/pods'):
            body = {'apiVersion': 'v1', 'kind': 'PodList', 'items': [pod]}
        elif self.path.startswith('/api/v1'):
            body = {'apiVersion': 'v1', 'kind': 'APIResourceList', 'groupVersion': 'v1',
                    'resources': [{'name': 'pods', 'singularName': 'pod', 'namespaced': True,
                                   'kind': 'Pod', 'verbs': ['get', 'list']}]}
        elif self.path.startswith('/apis'):
            body = {'apiVersion': 'v1', 'kind': 'APIGroupList', 'groups': []}
        else:
            body = {'apiVersion': 'v1', 'kind': 'APIVersions', 'versions': ['v1']}
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    kubectl = shutil.which('kubectl')
    if not kubectl:
        print('SKIP: installed kubectl unavailable for HTTPS integration; argv regression is in Rust')
        return
    harness = Harness('kubernetes')
    server = thread = None
    try:
        harness.until('old-target-row')
        root = harness.root
        config = root / 'openssl.cnf'
        config.write_text('[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n'
                          '[dn]\nCN=api.review.internal\n[ext]\n'
                          'subjectAltName=DNS:api.review.internal\nbasicConstraints=CA:TRUE\n')
        subprocess.run(['openssl', 'req', '-x509', '-nodes', '-newkey', 'rsa:2048',
                        '-keyout', str(root / 'key'), '-out', str(root / 'cert'),
                        '-days', '1', '-config', str(config)], check=True, capture_output=True, timeout=30)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Api)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(root / 'cert', root / 'key')
        server.socket = context.wrap_socket(server.socket, server_side=True)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        wrapper = root / 'bin/kubectl'
        wrapper.write_text(f'#!{sys.executable}\nimport os, sys\nos.execv({kubectl!r}, [{kubectl!r}] + sys.argv[1:])\n')
        query = (f'kubectl get pods --server https://127.0.0.1:{server.server_port} '
                 f'--certificate-authority {root}/cert --tls-server-name api.review.internal --token fixture')
        harness.send(b':' + query.encode() + b'\r', 'tls-pod')
        harness.send(b'\r', 'Exit code 0')
        assert 'name: tls-pod' in harness.screen.text(), harness.screen.text()
        print('PASS: installed kubectl list and selected inspect retain TLS certificate-name override')
    finally:
        harness.close()
        if server:
            server.shutdown()
            server.server_close()
        if thread:
            thread.join(timeout=5)


if __name__ == '__main__':
    main()
