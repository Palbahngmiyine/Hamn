"""Real PTY assertions shared by the opt-in workspace integration harness."""
import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import termios
import time
from terminal_screen import Screen


class Terminal:
    def __init__(self, binary, env, root):
        self.root = root
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 38, 160, 0, 0))
        self.before = termios.tcgetattr(self.slave)
        self.child = subprocess.Popen([binary, '--profile', 'verify'], env=env,
            stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True)
        self.screen = Screen(38, 160)
        self.record = (root / 'live-tui.ansi').open('ab')

    def until(self, marker, timeout=90):
        deadline = time.monotonic() + timeout
        while marker not in self.screen.text():
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([self.master], [], [], left)[0]:
                raise AssertionError((marker, self.screen.text()))
            data = os.read(self.master, 65536)
            self.record.write(data); self.record.flush(); self.screen.feed(data)
        (self.root / 'live-tui-screen.txt').write_text(self.screen.text())

    def send(self, data, marker=None):
        os.write(self.master, data)
        if marker: self.until(marker)

    def command(self, command, marker, code=0):
        self.send((':' + command + '\r').encode(), marker)
        self.until('Exit code ' + str(code))
        self.send(b'\r')

    def close(self):
        try:
            self.send(b'q')
            deadline = time.monotonic() + 10
            while self.child.poll() is None and time.monotonic() < deadline:
                if select.select([self.master], [], [], 0.1)[0]:
                    self.record.write(os.read(self.master, 65536))
            assert self.child.wait(timeout=1) == 0
            after = termios.tcgetattr(self.slave)
            for modes in (self.before, after): modes[3] &= ~getattr(termios, 'PENDIN', 0)
            assert self.before == after
        finally:
            if self.child.poll() is None: os.killpg(self.child.pid, signal.SIGKILL)
            self.child.wait(timeout=5)
            self.record.close(); os.close(self.master); os.close(self.slave)


def exercise(root, runtime, kubeconfig=None):
    env = dict(runtime.environment)
    if kubeconfig: env['KUBECONFIG'] = str(kubeconfig)
    terminal = Terminal(runtime.binary, env, root)
    try:
        if not (runtime.home / '.hamn/tui.json').exists():
            terminal.until('Choose your default workspace'); terminal.send(b'1\r')
        terminal.until('hamn-workspace-sentinel')
        direct = runtime.engine('ps', '--filter', 'name=hamn-workspace-sentinel', '--format', '{{.Names}}', profile='verify').strip()
        terminal.command("ps --filter name=hamn-workspace-sentinel --format '{{.Names}}'", direct)
        terminal.until('hamn-workspace-sentinel')
        terminal.send(b':exec -it hamn-workspace-sentinel sh\r', '/ #')
        terminal.send(b"printf 'PTY_INPUT_PROOF\\n'\r", 'PTY_INPUT_PROOF')
        terminal.send(b'\x10\x11', 'Exit code 1')  # Docker 29 exec reports its escape-sequence exit as 1
        terminal.send(b'\r', 'hamn-workspace-sentinel')
        terminal.command("exec hamn-workspace-sentinel sh -c 'exit 7'", 'Exit code 7', code=7)
        terminal.until('hamn-workspace-sentinel')
        if kubeconfig:
            terminal.send(b'\t', 'workspace-http')
            assert 'Hamn profile' not in terminal.screen.text()
            terminal.command('get pods -n workspace-proof -o name', 'pod/workspace-http')
            terminal.until('workspace-http')
            terminal.send(b':exec -it -n workspace-proof workspace-http -- sh\r', '/ #')
            terminal.send(b"printf 'KUBE_PTY_PROOF\\n'\r", 'KUBE_PTY_PROOF')
            terminal.send(b'exit\r', 'Exit code 0'); terminal.send(b'\r', 'workspace-http')
            terminal.command('apply -f ' + str(root / 'configmap.json'), 'configmap/tui-proof created')
            terminal.until('workspace-http')
            terminal.send(b':port-forward -n workspace-proof pod/workspace-http 18089:8080\r', 'Forwarding from 127.0.0.1:18089')
            import urllib.request
            assert urllib.request.urlopen('http://127.0.0.1:18089', timeout=10).read().strip() == b'kube-http-proof'
            terminal.send(b'\x03', 'Exit code'); terminal.send(b'\r', 'workspace-http')
    finally:
        terminal.close()
    print('PASS: real TUI command output, interactive exec/detach, exit status and restoration', flush=True)
