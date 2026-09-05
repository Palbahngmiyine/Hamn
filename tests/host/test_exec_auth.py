#!/usr/bin/env python3
"""Real exec plugins must honor deadlines, protect credentials, and avoid stdin."""
import http.server
import json
import os
from pathlib import Path
import shlex
import signal
import select
import subprocess
import tempfile
import threading
import time

binary = Path(os.environ.get('HAMN', 'target/debug/hamn')).resolve()
headers = []
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        headers.append(self.headers.get('Authorization'))
        body = b'{"apiVersion":"v1","kind":"PodList","items":[]}'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

with tempfile.TemporaryDirectory(prefix='hamn-exec-auth-', dir='/tmp') as directory:
    root = Path(directory)
    plugin, info, pid = (root / name for name in ['plugin', 'info', 'pid'])
    config = root / 'kubeconfig'
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    value = {'apiVersion':'v1','kind':'Config',
        'clusters':[{'name':'test','cluster':{'server':f'http://127.0.0.1:{server.server_port}'}}],
        'contexts':[{'name':'test','context':{'cluster':'test','user':'test'}}],
        'users':[{'name':'test','user':{'exec':{'apiVersion':'client.authentication.k8s.io/v1',
            'command':str(plugin),'interactiveMode':'IfAvailable','provideClusterInfo':True,
            'env':[{'name':'HAMN_AUTH_FIXTURE','value':'present'}]}}}]}
    def setup(script, interactive='IfAvailable'):
        plugin.write_text('#!/bin/sh\nset -eu\n' + script)
        plugin.chmod(0o700)
        value['users'][0]['user']['exec']['interactiveMode'] = interactive
        config.write_text(json.dumps(value))
    def invoke():
        before = config.read_bytes()
        result = subprocess.run([binary,'--headless','k8s','pods','list','--context','test',
            '--kubeconfig',config,'--timeout','2'],env=dict(os.environ, HOME=directory),
            capture_output=True,text=True,timeout=7)
        assert config.read_bytes() == before
        assert 'fixture-secret' not in result.stdout + result.stderr
        return result, json.loads(result.stdout)
    credential = {'apiVersion':'client.authentication.k8s.io/v1','kind':'ExecCredential',
        'status':{'token':'fixture-secret'}}
    try:
        setup('test ! -t 0\ntest "$HAMN_AUTH_FIXTURE" = present\n'
              + 'printf %s "$KUBERNETES_EXEC_INFO" > ' + shlex.quote(str(info)) + '\n'
              + 'printf %s ' + shlex.quote(json.dumps(credential)) + '\n')
        result, envelope = invoke()
        assert result.returncode == 0 and envelope['ok'], envelope
        assert headers == ['Bearer fixture-secret']
        supplied = json.loads(info.read_bytes())
        assert supplied['spec']['interactive'] is False
        assert supplied['spec']['cluster']['server'].startswith('http://127.0.0.1:')
        setup('echo fixture-secret >&2\nexit 1\n')
        assert invoke()[1]['error']['code'] == 'authenticationFailed'
        for status in [{'token':'fixture-secret','expirationTimestamp':'2000-01-01T00:00:00Z'},
                       {'clientCertificateData':'fixture-secret'}, {}]:
            setup('printf %s ' + shlex.quote(json.dumps(dict(credential,status=status))) + '\n')
            assert invoke()[1]['error']['code'] == 'authenticationFailed'
        setup('touch ' + shlex.quote(str(pid)) + '\n', interactive='Always')
        assert invoke()[1]['error']['code'] == 'authenticationRequired'
        assert not pid.exists()
        setup('echo $$ > ' + shlex.quote(str(pid)) + '\nexec /bin/sleep 60\n')
        started = time.monotonic()
        assert invoke()[1]['error']['code'] == 'timeout'
        assert time.monotonic() - started < 6
        child_pid = int(pid.read_text())
        try:
            os.kill(child_pid, 0)
        except ProcessLookupError:
            pass
        else:
            os.killpg(child_pid, signal.SIGKILL)
            raise AssertionError('timed out plugin was not reaped')
        fifo = root / 'ready'
        os.mkfifo(fifo, 0o600)
        setup('echo $$ > ' + shlex.quote(str(fifo)) + '\nexec /bin/sleep 60\n')
        fd = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
        operation = subprocess.Popen([binary,'--headless','k8s','pods','list','--context','test',
            '--kubeconfig',config,'--timeout','30'], env=dict(os.environ, HOME=directory),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            assert select.select([fd], [], [], 5)[0], 'plugin did not start'
            child_pid = int(os.read(fd, 64))
            operation.send_signal(signal.SIGTERM)
            output, error = operation.communicate(timeout=5)
            assert json.loads(output)['error']['code'] == 'cancelled', (output,error)
            try: os.kill(child_pid, 0)
            except ProcessLookupError: pass
            else: raise AssertionError('cancelled plugin was not reaped')
        finally:
            os.close(fd)
            if operation.poll() is None: operation.kill()
            operation.wait(timeout=5)
        assert headers == ['Bearer fixture-secret']
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
print('Exec authentication credentials, noninteractive mode, timeout and cleanup: passed')
