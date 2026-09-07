#!/usr/bin/env python3
"""Exercise the real TUI and server-side delete preconditions on a local HTTP fixture."""
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from terminal_screen import Screen

BINARY = Path(os.environ.get('HAMN', 'build/hamn')).resolve()
REAL_KUBECTL = shutil.which('kubectl')
requests = []
identity = {'uid': 'selected-uid', 'resourceVersion': '42'}


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_DELETE(self):
        if self.headers.get('Transfer-Encoding') == 'chunked':
            data = bytearray()
            while True:
                size = int(self.rfile.readline().strip(), 16)
                if not size:
                    assert self.rfile.readline() == b'\r\n'
                    break
                data.extend(self.rfile.read(size))
                assert self.rfile.read(2) == b'\r\n'
        else:
            data = self.rfile.read(int(self.headers['Content-Length']))
        body = json.loads(data)
        requests.append((self.path, body))
        matches = body.get('preconditions') == identity
        status = 200 if matches else 409
        data = json.dumps({'apiVersion': 'v1', 'kind': 'Status',
            'status': 'Success' if matches else 'Failure',
            'reason': 'Success' if matches else 'Conflict', 'code': status,
            'message': 'DELETE_ACCEPTED' if matches else 'REPLACEMENT_PRESERVED'}).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def scenario(changed):
    requests.clear()
    identity.update(uid='selected-uid', resourceVersion='42')
    with tempfile.TemporaryDirectory(prefix='hamn-guarded-delete-', dir='/tmp') as directory:
        root = Path(directory)
        (root / 'bin').mkdir()
        (root / '.hamn').mkdir(mode=0o700)
        prefs = root / '.hamn/tui.json'
        prefs.write_text('{"version":1,"defaultWorkspace":"kubernetes"}')
        prefs.chmod(0o600)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        endpoint = f'http://127.0.0.1:{server.server_port}'
        config = root / 'config'
        config.write_text(json.dumps({'apiVersion':'v1', 'kind':'Config', 'current-context':'fixture',
            'contexts':[{'name':'fixture','context':{'cluster':'fixture','namespace':'test'}}],
            'clusters':[{'name':'fixture','cluster':{'server':endpoint}}]}))
        fixture = root / 'bin/kubectl'
        fixture.write_text(f'#!{sys.executable}\n' + '''import json,os,sys,urllib.request,urllib.error
args=sys.argv[1:]
if 'get' in args:
 print(json.dumps({'items':[{'apiVersion':'v1','kind':'Pod','metadata':{'name':'victim','namespace':'test','uid':'selected-uid','resourceVersion':'42'}}]}))
else:
 assert 'delete' in args and '--raw' in args and '--filename' in args,args
 path=args[args.index('--filename')+1]
 assert path.startswith('/dev/fd/')
 with open(path,'rb') as body: assert os.fstat(body.fileno()).st_nlink==0
 if os.environ.get('REAL_KUBECTL'):
  os.execv(os.environ['REAL_KUBECTL'],[os.environ['REAL_KUBECTL']]+args)
 data=open(path,'rb').read()
 request=urllib.request.Request(os.environ['FIXTURE_ENDPOINT']+args[args.index('--raw')+1],data=data,method='DELETE',headers={'Content-Type':'application/json'})
 try:
  print(urllib.request.urlopen(request,timeout=5).read().decode())
 except urllib.error.HTTPError as error:
  print(error.read().decode());sys.exit(1)
''')
        fixture.chmod(0o755)
        env = dict(os.environ, HOME=str(root), KUBECONFIG=str(config), PATH=f'{root}/bin:/usr/bin:/bin',
                   TERM='xterm-256color', REAL_KUBECTL=REAL_KUBECTL or '', FIXTURE_ENDPOINT=endpoint)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 140, 0, 0))
        screen = Screen(32, 140)
        child = subprocess.Popen([BINARY], env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        def until(marker):
            deadline = time.monotonic() + 15
            while marker not in screen.text():
                left = deadline - time.monotonic()
                assert left > 0 and select.select([master], [], [], left)[0], screen.text()
                screen.feed(os.read(master, 65536))
        try:
            until('victim')
            os.write(master, b'd')
            until('Confirm delete pods victim')
            if changed:
                identity[changed] = 'replacement'
            os.write(master, b'y')
            until('REPLACEMENT_PRESERVED' if changed else 'DELETE_ACCEPTED')
            until('Exit code 1' if changed else 'Exit code 0')
            assert requests == [('/api/v1/namespaces/test/pods/victim', {
                'apiVersion':'v1', 'kind':'DeleteOptions',
                'preconditions':{'uid':'selected-uid','resourceVersion':'42'}})], requests
        finally:
            child.send_signal(signal.SIGTERM)
            try:
                child.wait(timeout=5)
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=5)
                os.close(master)
                os.close(slave)
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)


for changed in (None, 'uid', 'resourceVersion'):
    scenario(changed)
print('PASS: selected UID/version guarded at DELETE server; transport=' + ('installed kubectl' if REAL_KUBECTL else 'fixture'))
