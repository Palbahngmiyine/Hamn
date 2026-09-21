#!/usr/bin/env python3
"""Consume unchanged publisher outputs through an isolated HTTPS fixture.

The CONNECT endpoint serves only the exact declared artifact paths; it never
forwards to the public network. Trust and proxy settings belong to test children.
Production curl, TLS hostname verification, manifests and installer are unchanged.
"""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import threading
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def consume(published, candidate, work):
    work.mkdir(mode=0o700)
    metadata = [published / name for name in ('hamn-update-manifest.json', 'hamn-update-manifest-v3.json')]
    hashes = {str(path): digest(path) for path in metadata}
    manifests = [json.loads(path.read_text()) for path in metadata]
    allowed = {}
    for manifest in manifests:
        for artifact in manifest['artifacts'].values():
            url = urlsplit(artifact['url'])
            assert url.scheme == 'https' and url.netloc == 'github.com' and not url.query
            payload = candidate / Path(url.path).name
            assert payload.is_file() and digest(payload) == artifact['sha256']
            allowed[url.path] = payload
    config, cert, key = (work / name for name in ('tls.conf', 'cert.pem', 'key.pem'))
    config.write_text('[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n'
        '[dn]\nCN=github.com\n[ext]\nsubjectAltName=DNS:github.com\n'
        'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyEncipherment,keyCertSign\n'
        'extendedKeyUsage=serverAuth\n')
    subprocess.run(['/usr/bin/openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
        '-days', '1', '-config', config, '-keyout', key, '-out', cert],
        check=True, capture_output=True, timeout=30)
    key.chmod(0o600)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(cert, key)
    requests, failures = [], []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_CONNECT(self):
            if self.path != 'github.com:443':
                failures.append(self.path)
                self.send_error(403)
                return
            self.send_response(200)
            self.end_headers()
            self.wfile.flush()
            self.connection.settimeout(10)
            try:
                self.connection = context.wrap_socket(self.connection, server_side=True)
                self.rfile = self.connection.makefile('rb')
                self.wfile = self.connection.makefile('wb')
                self.close_connection = True
                self.handle_one_request()
            except (OSError, ssl.SSLError) as error:
                failures.append(str(error))
            finally:
                self.close_connection = True

        def do_GET(self):
            if self.path not in allowed or not isinstance(self.connection, ssl.SSLSocket):
                failures.append(self.path)
                self.send_error(404)
                return
            payload = allowed[self.path]
            requests.append(self.path)
            self.send_response(200)
            self.send_header('Content-Length', str(payload.stat().st_size))
            self.send_header('Connection', 'close')
            self.end_headers()
            with payload.open('rb') as source:
                shutil.copyfileobj(source, self.wfile, 64 * 1024)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.daemon_threads = False
    worker = threading.Thread(target=server.serve_forever)
    worker.start()
    proxy = 'http://127.0.0.1:' + str(server.server_address[1])
    env = {**os.environ, 'HTTPS_PROXY': proxy, 'https_proxy': proxy,
           'NO_PROXY': '', 'no_proxy': '', 'CURL_CA_BUNDLE': str(cert),
           'SSL_CERT_FILE': str(cert), 'HAMN_NO_UPDATE_CHECK': '1',
           'HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS': '1'}

    def command(home, *args):
        result = subprocess.run(list(map(str, args)), env={**env, 'HOME': str(home)},
                                capture_output=True, text=True, timeout=90)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result

    try:
        for schema, manifest in zip((2, 3), metadata):
            home = work / ('home-v' + str(schema))
            home.mkdir(mode=0o700)
            before = len(requests)
            args = ['/bin/bash', ROOT / 'scripts/update-host.sh', '--bootstrap', '--output-json',
                    '--bindir', home / 'bin', '--datadir', home / 'src', '--manifest', manifest]
            result = json.loads(command(home, *args).stdout)
            assert result['completed'] and result['latestVersion'] == '0.0.1'
            assert result['profileDisksChanged'] is False and len(requests) - before == 2
            assert set(requests[before:]) == set(allowed)
            count = len(requests)
            assert json.loads(command(home, *args).stdout)['status'] == 'up-to-date'
            assert len(requests) == count, 'healthy repeat made a payload request'
        home = work / 'home-installer'
        home.mkdir(mode=0o700)
        cache = home / '.hamn/cache'
        cache.parent.mkdir(mode=0o700)
        cache.mkdir(mode=0o755)
        before = len(requests)
        # The installer intentionally clears proxy/CA environment variables.
        # Acquire the exact published bytes through native TLS first, then prove
        # the unchanged installer consumes them with networking denied by the OS.
        native = work / 'home-v3/bin/hamn'
        for name in ('host', 'guestImage'):
            command(home, native, '__install-support', 'upgrade', 'acquire',
                    metadata[1], name, cache, work / (name + '-counts.json'))
        assert len(requests) - before == 2 and set(requests[before:]) == set(allowed)
        before = len(requests)
        command(home, '/usr/bin/sandbox-exec', '-p', '(version 1)(allow default)(deny network*)',
                '/bin/bash', candidate / 'install.sh')
        assert command(home, home / '.local/bin/hamn', '--version').stdout.strip() == 'hamn 0.0.1'
        assert len(requests) == before
        assert not failures, failures
        assert {str(path): digest(path) for path in metadata} == hashes
        print('PASS: exact published v2/v3 consumed by native HTTPS; repeats and unchanged cached installer require no network')
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)
        assert not worker.is_alive()


if __name__ == '__main__':
    consume(*(Path(value).resolve() for value in sys.argv[1:]))
