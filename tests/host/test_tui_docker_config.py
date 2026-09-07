#!/usr/bin/env python3
"""Context reload must keep the installed Docker CLI's effective config directory."""
import http.server
import json
import os
import shlex
import shutil
import socketserver
import subprocess
import threading
from test_tui_native_regressions import Harness


def main():
    docker = shutil.which('docker')
    if not docker:
        print('SKIP: installed Docker unavailable; config reload parsing is covered in Rust')
        return
    harness = Harness('containers')
    servers, threads, requests = [], [], []

    def handler(label):
        class Api(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_HEAD(self):
                self.do_GET()

            def do_GET(self):
                if self.path.endswith('/_ping'):
                    body = b'OK'
                elif '/containers/json' in self.path:
                    requests.append(label)
                    body = json.dumps([{'Id': 'a' * 64, 'Names': ['/' + label + '-endpoint-row'],
                        'Image': 'fixture', 'State': 'running', 'Status': 'Up', 'Ports': [],
                        'Labels': {}, 'Created': 0}]).encode()
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header('API-Version', '1.47')
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(body)
        return Api

    try:
        harness.until('old-target-row')
        env = {k: v for k, v in os.environ.items() if not k.startswith('DOCKER_')}
        env.update(HOME=str(harness.root), DOCKER_API_VERSION='1.47')
        for label in ('first', 'last', 'context'):
            config = harness.root / label
            config.mkdir()
            endpoint = config / 'api.sock'
            server = socketserver.UnixStreamServer(str(endpoint), handler(label))
            servers.append(server)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            threads.append(thread)
            for command in (['create', 'shared', '--docker', 'host=unix://' + str(endpoint)], ['use', 'shared']):
                subprocess.run([docker, '--config', str(config), 'context'] + command,
                    env=env, capture_output=True, check=True, timeout=10)
        wrapper = harness.root / 'bin/docker'
        wrapper.write_text('#!' + os.sys.executable + '\nimport os, sys\n'
            + 'for key in list(os.environ):\n    if key.startswith("DOCKER_"): del os.environ[key]\n'
            + 'os.environ["DOCKER_API_VERSION"] = "1.47"\n'
            + f'os.chdir({str(harness.root)!r})\nos.execv({docker!r}, [{docker!r}] + sys.argv[1:])\n')
        for flags, label in [
            (['--config', str(harness.root / 'first'), '--config', str(harness.root / 'last')], 'last'),
            (['--config', 'context'], 'context'),
            (['--config', str(harness.root / 'last'), '--config=' + str(harness.root / 'first')], 'first'),
        ]:
            direct = subprocess.run([docker] + flags + ['ps', '--format', '{{json .}}'],
                env=env, cwd=harness.root, capture_output=True, text=True, check=True, timeout=10)
            assert label + '-endpoint-row' in direct.stdout, direct.stdout
            command = shlex.join(['docker'] + flags + ['context', 'show'])
            harness.send(b':' + command.encode() + b'\r', 'Exit code 0')
            harness.send(b'\r', label + '-endpoint-row')
            assert requests[-1] == label, requests
            print('PASS: context reload retains', label, 'endpoint like installed Docker')
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
