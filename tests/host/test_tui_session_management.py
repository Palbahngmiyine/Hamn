#!/usr/bin/env python3
"""Detached CLI sessions keep running; browser/list controls retain owned cleanup."""
import json
import os
import select
import sys
import time
from test_tui_native_regressions import Harness

CONTROL_B = b'\x1b\x02'
CONTROL_S = b'\x1b\x13'


def process_alive(pid):
    try: os.kill(pid, 0); return True
    except ProcessLookupError: return False


def wait_dead(pid):
    deadline = time.monotonic() + 5
    while process_alive(pid):
        assert time.monotonic() < deadline, f'owned process survived: {pid}'
        select.select([], [], [], 0.01)


def sessions():
    harness = Harness('kubernetes')
    try:
        harness.until('old-target-row')
        peer = harness.root / 'bin/kubectl'
        source = peer.read_text()
        source = source.replace("elif 'events' in args:", """elif 'port-forward' in args:
    import subprocess
    helper = subprocess.Popen(['/bin/sleep','600'])
    with (root/'session-pids').open('a') as out:
        out.write(json.dumps([os.getpid(),helper.pid])+'\\n')
    print('FORWARD_READY:'+args[-1], flush=True)
    while True:
        value=sys.stdin.readline()
        if not value: break
        print('RECEIVED:'+value.strip(),flush=True)
elif 'events' in args:""")
        peer.write_text(source)
        for mapping in ('8080:80', '9090:90'):
            harness.send(f':port-forward --token session-credential-fixture pod/example {mapping}\r'.encode(), f'FORWARD_READY:{mapping}')
            harness.send(CONTROL_B, 'old-target-row')
        pids = [json.loads(line) for line in (harness.root/'session-pids').read_text().splitlines()]
        assert len(pids) == 2 and all(process_alive(pid) for pair in pids for pid in pair)
        harness.send(CONTROL_S, 'Sessions: Enter resumes')
        harness.until('port-forward pod/example 8080:80')
        harness.until('port-forward pod/example 9090:90')
        assert 'session-credential-fixture' not in harness.screen.text()
        assert 'session-credential-fixture' not in (harness.root / '.hamn/tui.json').read_text()
        calls = [args for _, args in harness.calls() if 'port-forward' in args]
        assert len(calls) == 2 and all(args[args.index('--token') + 1] == 'session-credential-fixture' for args in calls)
        # The last detached session is selected; its PTY still accepts normal input.
        harness.send(b'\r', 'FORWARD_READY:9090:90')
        harness.send(b'ordinary-input\r', 'RECEIVED:ordinary-input')
        harness.send(CONTROL_S, 'Sessions: Enter resumes')
        os.write(harness.master, b'd')
        for pid in pids[-1]: wait_dead(pid)
        assert all(process_alive(pid) for pid in pids[0]), pids
        harness.send(b'\x1b', 'old-target-row')
    finally:
        harness.close()
    for pid in pids[0]: wait_dead(pid)


def query_timeout():
    harness = Harness('kubernetes')
    try:
        harness.until('old-target-row')
        harness.send(b':refresh-timeout 1\r', 'old-target-row')
        peer = harness.root / 'bin/kubectl'
        source = peer.read_text()
        source = source.replace("elif 'ps' in args or 'get' in args:", """elif 'ps' in args or 'get' in args:
    import subprocess
    helper = subprocess.Popen(['/bin/sleep','600'])
    (root/'query-pids').write_text(json.dumps([os.getpid(),helper.pid]))
    with (root/'notice').open('w') as out: out.write('timeout-query-started\\n'); out.flush()
    with (root/'gate').open() as gate: gate.read(1)""")
        peer.write_text(source)
        os.write(harness.master, b'R')
        harness.wait(lambda: b'timeout-query-started' in harness.notices)
        pids = json.loads((harness.root/'query-pids').read_text())
        harness.send(b':INPUT_RESPONSIVE', ':INPUT_RESPONSIVE')
        os.write(harness.master, b'\x1b')
        harness.until('Query exceeded 1 seconds')
        for pid in pids: wait_dead(pid)
        harness.until('retry backoff')
        harness.send(b'p', 'Paused')
    finally:
        harness.close()


if __name__ == '__main__':
    sessions(); query_timeout()
    print('PASS: multiple PTY sessions, foreground input, owned group cleanup and responsive query deadlines')
