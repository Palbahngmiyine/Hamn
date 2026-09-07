#!/usr/bin/env python3
"""Invalid settings and hidden-workspace uncertainty remain visible in the real TUI."""
import http.server
import json
import os
import threading
from test_tui_native_regressions import Harness


def invalid_preferences_return_to_choice(fifo):
    def prepare(path):
        if fifo:
            path.unlink()
            os.mkfifo(path, 0o600)
        else:
            path.write_text('{broken')
    harness = Harness('containers', prepare)
    try:
        harness.until('Choose your default workspace')
        assert 'Invalid or unsafe' in harness.screen.text(), harness.screen.text()
        harness.send(b'1\r', 'old-target-row')
        prefs = harness.root / '.hamn/tui.json'
        assert prefs.is_file()
        assert json.loads(prefs.read_text()) == {'version': 1, 'defaultWorkspace': 'containers'}
        assert prefs.stat().st_mode & 0o777 == 0o600
    finally:
        harness.close()


def hidden_workspace_warning_survives_cancel_and_exit():
    accepted, release = threading.Event(), threading.Event()
    requests = []

    class Api(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            body = json.dumps({'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {
                'name': 'sample', 'namespace': 'test', 'uid': 'uid-original',
                'resourceVersion': '1'}}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_DELETE(self):
            requests.append((self.path, json.loads(self.rfile.read(int(self.headers['Content-Length'])))))
            accepted.set()
            # Hold the accepted request until the frontend finishes cancellation.
            release.wait(20)

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Api)
    server.daemon_threads = False
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    harness = Harness('kubernetes')
    try:
        harness.until('old-target-row')
        config = harness.root / 'kubeconfig'
        content = json.loads(config.read_text())
        content['clusters'][0]['cluster']['server'] = f'http://127.0.0.1:{server.server_port}'
        config.write_text(json.dumps(content))
        command = f'k8s pods delete sample --context old-cluster --namespace test --kubeconfig {config} --uid uid-original'
        harness.send(b':' + command.encode() + b'\r', 'Impact:')
        os.write(harness.master, b'y')
        assert accepted.wait(5), harness.screen.text()
        harness.send(b'\t', '[Containers]')
        harness.send(b'q', 'Cancel the active operation and exit?')
        harness.output.clear()
        os.write(harness.master, b'y')
        # Exit warnings follow terminal restoration, outside Ratatui draw frames.
        harness.wait(lambda: b'outcomeUnknown' in harness.output and b'uid-original' in harness.output)
        assert harness.child.wait(timeout=5) == 0
        text = harness.output.decode()
        for value in ('k8s pods delete', 'sample', 'uid-original', 'Inspect it before retrying'):
            assert value in text, text
        assert len(requests) == 1, requests
        assert requests[0][1]['preconditions'] == {'uid': 'uid-original', 'resourceVersion': '1'}
        assert sorted(p.name for p in (harness.root / '.hamn').iterdir()) == ['tui.json']
    finally:
        release.set()
        harness.close()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()


if __name__ == '__main__':
    for fifo in (False, True):
        invalid_preferences_return_to_choice(fifo)
    hidden_workspace_warning_survives_cancel_and_exit()
    print('PASS: malformed/FIFO settings recover; hidden-workspace cancellation reports its unknown target')
