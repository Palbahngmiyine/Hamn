#!/usr/bin/env python3
"""Compare the TUI all toggle with installed Docker against an isolated Unix API."""
import http.server
import json
import os
import shutil
import socketserver
import subprocess
import threading
import urllib.parse

from test_tui_native_regressions import Harness


def main():
    docker = shutil.which('docker')
    if not docker:
        print('SKIP: installed Docker unavailable; all-flag parsing is covered in Rust')
        return
    harness = Harness('containers')
    server = None
    thread = None
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_HEAD(self):
            self.do_GET()

        def do_GET(self):
            url = urllib.parse.urlsplit(self.path)
            if url.path.endswith('/_ping'):
                body = b'OK'
            elif url.path.endswith('/containers/json'):
                query = urllib.parse.parse_qs(url.query)
                requests.append(query)
                name = 'all' if query.get('all') == ['1'] else 'running'
                name += '-size' if query.get('size') == ['1'] else '-plain'
                body = json.dumps([{'Id': 'a' * 64, 'Names': ['/' + name],
                    'Image': 'fixture', 'ImageID': 'b' * 64, 'Command': 'fixture',
                    'Created': 0, 'State': 'running', 'Status': 'Up', 'Ports': [],
                    'Labels': {}, 'SizeRw': 42, 'SizeRootFs': 100}]).encode()
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

    try:
        harness.until('old-target-row')
        profile = harness.root / '.hamn/default'
        profile.mkdir(mode=0o700, exist_ok=True)
        endpoint = profile / 'docker.sock'
        server = socketserver.UnixStreamServer(str(endpoint), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        # Replace only this fixture's CLI peer; never inherit a user's Docker target.
        wrapper = harness.root / 'bin/docker'
        wrapper.write_text('#!' + os.sys.executable + '\nimport os, sys\n'
            + 'for key in list(os.environ):\n    if key.startswith("DOCKER_"): del os.environ[key]\n'
            + 'os.environ["DOCKER_API_VERSION"] = "1.47"\n'
            + 'os.execv(' + repr(docker) + ', [' + repr(docker) + '] + sys.argv[1:])\n')
        env = {k: v for k, v in os.environ.items() if not k.startswith('DOCKER_')}
        env.update(HOME=str(harness.root), DOCKER_API_VERSION='1.47')
        cases = [
            (['-as'], True),
            (['-sa=false'], False),
            (['-a=true'], True),
            (['--all=true', '--all=false'], False),
            (['--all=false', '-as'], True),
            (['-s', '-f', 'label=app=api'], False),
            (['-sf', 'label=app=api'], False),
            (['-asf', 'label=app=api'], True),
            (['--all=TRUE'], True),
            (['-a=0'], False),
            (['-asn5'], True),
            (['--size', '--size=false'], False),
            (['--size=false', '-s'], False),
            (['-as=false'], True),
        ]
        for case_index, (flags, all_value) in enumerate(cases):
            command = ['ps'] + flags
            direct_marker = 'direct=' + str(case_index)
            direct = subprocess.run([docker, '--host', 'unix://' + str(endpoint)]
                + command + ['--filter', 'label=' + direct_marker, '--format', '{{json .}}'], env=env,
                capture_output=True, text=True, timeout=10)
            assert direct.returncode == 0, (command, direct.stderr)
            baseline = next(query for query in reversed(requests)
                if direct_marker in json.loads(query.get('filters', ['{}'])[0]).get('label', []))
            assert (baseline.get('all') == ['1']) == all_value, (command, baseline)
            size = baseline.get('size') == ['1']
            displayed_size = json.loads(direct.stdout)['Size']
            plain = subprocess.run([docker, '--host', 'unix://' + str(endpoint)] + command,
                env=env, capture_output=True, text=True, timeout=10)
            assert plain.returncode == 0, plain.stderr
            # Docker's JSON formatter evaluates Size even without --size. The
            # ordinary CLI header establishes whether the user requested it.
            show_size = 'SIZE' in plain.stdout.splitlines()[0].split()
            # Include a sequence marker in the filter so a stale screen cannot pass.
            marker = 'label=case=' + str(case_index)
            text = 'docker ps ' + ' '.join(flags) + ' --filter ' + marker
            os.write(harness.master, (':' + text + '\r').encode())
            def observed(value):
                query = requests[-1]
                return ((query.get('all') == ['1']) == value
                    and marker[6:] in json.loads(query.get('filters', ['{}'])[0]).get('label', []))
            harness.wait(lambda: observed(all_value))
            suffix = '-size' if size else '-plain'
            for value in (not all_value, all_value, not all_value):
                os.write(harness.master, b'a')
                harness.wait(lambda: observed(value))
                harness.until(('all' if value else 'running') + suffix)
                screen = harness.screen.text()
                assert ('Size' in screen) == show_size, (command, screen)
                if show_size:
                    assert displayed_size in screen, (command, displayed_size, screen)
                query = requests[-1]
                assert (query.get('size') == ['1']) == size, (command, query)
                assert query.get('limit') == baseline.get('limit'), (command, query)
                if 'label=app=api' in flags:
                    assert 'app=api' in json.loads(query['filters'][0])['label']
            print('PASS:', ' '.join(command), 'direct CLI and three TUI toggles')
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
