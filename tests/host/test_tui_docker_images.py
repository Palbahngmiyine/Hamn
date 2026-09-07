#!/usr/bin/env python3
"""Preserve installed Docker image display options against an owned Unix API."""
import fcntl
import http.server
import json
import os
import shutil
import socketserver
import struct
import subprocess
import termios
import threading
import urllib.parse
from test_tui_native_regressions import Harness, Screen


def main():
    docker = shutil.which('docker')
    if not docker:
        print('SKIP: installed Docker unavailable for image display comparison')
        return
    harness = Harness('containers')
    server = None
    thread = None
    image_id, digest = 'sha256:' + 'a' * 64, 'sha256:' + 'b' * 64

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_HEAD(self):
            self.do_GET()

        def do_GET(self):
            path = urllib.parse.urlsplit(self.path).path
            if path.endswith('/_ping'):
                body = b'OK'
            elif path.endswith('/images/json'):
                body = json.dumps([{'Id': image_id, 'RepoTags': ['digest-fixture:latest'],
                    'RepoDigests': ['digest-fixture@' + digest], 'Created': 0,
                    'Size': 4096, 'SharedSize': 0, 'VirtualSize': 4096,
                    'Labels': {}, 'Containers': 0}]).encode()
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('API-Version', '1.47')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            if self.command != 'HEAD':
                self.wfile.write(body)

    try:
        harness.until('old-target-row')
        root = harness.root
        endpoint = root / 'images.sock'
        server = socketserver.UnixStreamServer(str(endpoint), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        wrapper = root / 'bin/docker'
        wrapper.write_text('#!' + os.sys.executable + '\nimport json, os, sys\n'
            + 'from pathlib import Path\n'
            + 'with Path(os.environ["HOME"], "native-calls").open("a") as out:\n'
            + '    out.write(json.dumps(sys.argv[1:]) + "\\n")\n'
            + 'for key in list(os.environ):\n    if key.startswith("DOCKER_"): del os.environ[key]\n'
            + 'os.environ["DOCKER_API_VERSION"] = "1.47"\n'
            + f'os.execv({docker!r}, [{docker!r}] + sys.argv[1:])\n')
        env = {k: v for k, v in os.environ.items() if not k.startswith('DOCKER_')}
        env.update(HOME=str(root), DOCKER_API_VERSION='1.47')
        target = ['--host', 'unix://' + str(endpoint)]
        # Keep complete CLI lines visible; resize mechanics have separate PTY tests.
        harness.screen = Screen(32, 320)
        fcntl.ioctl(harness.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 320, 0, 0))
        harness.send((':docker ' + ' '.join(target) + ' images\r').encode(), 'digest-fixture')
        assert 'docker terminal' not in harness.screen.text()
        for command in ['images --digests', 'image ls --digests', 'image list --digests',
                        'images --digests=false', 'images --digests=true --digests=false',
                        'images --digests=false --digests', 'images --no-trunc',
                        'images --digests --no-trunc', 'images --tree']:
            args = target + command.split()
            direct = subprocess.run([docker] + args, env=env, capture_output=True, text=True, timeout=10)
            if '--tree' not in command:
                assert direct.returncode == 0, direct.stderr
            calls = lambda: [json.loads(line) for line in (root / 'native-calls').read_text().splitlines()]
            before = len(calls())
            harness.send((':docker ' + ' '.join(args) + '\r').encode(), f'Exit code {direct.returncode}')
            screen = harness.screen.text()
            # Returning from the preceding terminal refreshes the original list.
            refresh = target + ['images', '--format', '{{json .}}']
            assert [call for call in calls()[before:] if call != refresh] == [args], calls()[before:]
            assert 'docker terminal' in screen, screen
            for line in (direct.stdout + direct.stderr).splitlines():
                assert ' '.join(line.split()) in ' '.join(screen.split()), (command, line, screen)
            if digest in direct.stdout:
                assert digest in screen, screen
            if '--no-trunc' in command:
                assert image_id in screen, screen
            harness.send(b'\r', '[Containers]')
            print('PASS:', command, 'matches installed CLI output and exit', direct.returncode)
        print('PASS: installed Docker digests/full IDs/tree retain original argv, output and exit code; ordinary images remain selectable')
    finally:
        harness.close()
        if server:
            server.shutdown()
            server.server_close()
        if thread:
            thread.join(timeout=5)
            assert not thread.is_alive()


if __name__ == '__main__':
    main()
