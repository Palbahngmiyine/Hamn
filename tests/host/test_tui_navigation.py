#!/usr/bin/env python3
"""Direct TUI reads return to their list after Esc; all state is disposable."""
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time

binary = Path(os.environ.get('HAMN', 'target/debug/hamn')).resolve()


def exercise(action):
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            requests.append(self.path)
            path = self.path.split('?')[0]
            obj = {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {
                'name': 'sample', 'namespace': 'default', 'uid': 'original',
                'resourceVersion': '10', 'annotations': {'marker': 'detail-response-seen'}}}
            if path.endswith('/log'):
                body = b'detail-response-seen\n'
            elif path.endswith('/pods'):
                obj['metadata']['name'] = 'returned-list-row'
                body = json.dumps({'apiVersion': 'v1', 'kind': 'PodList', 'items': [obj]}).encode()
            else:
                body = json.dumps(obj).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='hamn-tui-navigation-', dir='/tmp') as directory:
            config = Path(directory) / 'config'
            config.write_text(json.dumps({'apiVersion': 'v1', 'kind': 'Config',
                'contexts': [{'name': 'dev', 'context': {'cluster': 'dev', 'namespace': 'default'}}],
                'clusters': [{'name': 'dev', 'cluster': {'server': f'http://127.0.0.1:{server.server_port}'}}]}))
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 120, 0, 0))
            child = subprocess.Popen([binary], stdin=slave, stdout=slave, stderr=slave,
                env=dict(os.environ, HOME=directory, KUBECONFIG=str(config), TERM='xterm-256color'),
                start_new_session=True)
            output = bytearray()

            def until(marker):
                deadline = time.monotonic() + 10
                while marker not in output:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not select.select([master], [], [], remaining)[0]:
                        raise AssertionError((action, marker, requests, bytes(output[-3000:])))
                    output.extend(os.read(master, 65536))

            try:
                until(b'Hamn')
                os.write(master, f':k8s pods {action} sample --context dev --namespace default\r'.encode())
                until(b'detail-response-seen')
                original = list(requests)
                assert sum('/pods/sample/log?' in path for path in original) == (action == 'logs')
                assert sum(path.endswith('/pods/sample') for path in original) == 1
                output.clear()
                os.write(master, b'\x1b')
                until(b'returned-list-row')
                later = requests[len(original):]
                assert later and all(path.split('?')[0].endswith('/pods') for path in later), later
                assert sum('/pods/sample/log?' in path for path in requests) == (action == 'logs')
                assert sum(path.endswith('/pods/sample') for path in requests) == 1
                os.write(master, b'q')
                until(b'\x1b[?1049l')
                assert child.wait(timeout=5) == 0
                assert not (Path(directory) / '.hamn').exists()
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=5)
                os.close(master)
                os.close(slave)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()


for operation in ('logs', 'inspect'):
    exercise(operation)
print('TUI direct logs/inspect return to the list without replaying reads: passed')
