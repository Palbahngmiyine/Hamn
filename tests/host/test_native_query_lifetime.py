#!/usr/bin/env python3
"""Structured CLI queries own their process group until completion or cancellation."""
import os
import select
import sys
from test_tui_native_regressions import Harness

CHILD = '''import os, sys
from pathlib import Path
root = Path(os.environ['FIXTURE_ROOT'])
with (root / 'lifetime').open('wb', buffering=0) as lifetime:
    lifetime.write(b'R')
    with (root / 'notice').open('w') as notice:
        notice.write('child-ready\\n'); notice.flush()
    os.write(int(sys.argv[1]), b'1')
    os.close(int(sys.argv[1]))
    with (root / 'gate').open() as gate:
        gate.read(1)
'''
PEER = '''if any(arg.startswith('lifetime-') for arg in args):
    import subprocess
    ready, notify = os.pipe()
    child = subprocess.Popen([sys.executable, str(root / 'child.py'), str(notify)], pass_fds=(notify,))
    os.close(notify)
    assert os.read(ready, 1) == b'1'
    os.close(ready)
    if 'lifetime-regroup' in args:
        os.setpgid(0, os.getpgid(os.getppid()))
        with (root / 'notice').open('w') as notice:
            notice.write('parent-regrouped\\n'); notice.flush()
    if 'lifetime-exit' in args:
        print(json.dumps({'ID':'completed', 'Names':'completed-query'}), flush=True)
        os._exit(0)
    if 'lifetime-error' in args:
        os._exit(7)
    if 'lifetime-overflow' in args or 'lifetime-stderr-overflow' in args:
        output = sys.stderr if 'lifetime-stderr-overflow' in args else sys.stdout
        output.write('x' * (17 * 1024 * 1024)); output.flush()
    child.wait()
    sys.exit(0)
elif 'config' in args:'''


def exercise(mode):
    harness = Harness('containers')
    lifetime = None
    try:
        harness.until('old-target-row')
        os.mkfifo(harness.root / 'lifetime')
        lifetime = os.open(harness.root / 'lifetime', os.O_RDONLY | os.O_NONBLOCK)
        (harness.root / 'child.py').write_text(CHILD)
        peer = harness.root / 'bin/docker'
        peer.write_text(peer.read_text().replace("if 'config' in args:", PEER))
        os.write(harness.master, f':ps --filter lifetime-{mode}\r'.encode())
        harness.wait(lambda: (b'parent-regrouped' if mode == 'regroup' else b'child-ready') in harness.notices)
        assert os.read(lifetime, 1) == b'R'
        if mode in ('cancel', 'regroup'):
            harness.send(b':version\r', 'ACTION_DONE')
            harness.until('Exit code 0')
        else:
            # Hold auto-refresh without cancelling the current query. A second
            # query must not open another writer while checking this one's EOF.
            if mode.endswith('overflow'):
                harness.until('CLI output exceeds 16 MiB')
            elif mode == 'error':
                harness.until('exit status: 7')
            harness.send(b':LIFETIME_BARRIER', ':LIFETIME_BARRIER')
        # Only the descendant holds the write end. EOF proves that cancellation,
        # parent exit, and output-limit failures close it, without PID polling.
        assert select.select([lifetime], [], [], 5)[0], f'{mode}: query child survived'
        assert os.read(lifetime, 1) == b'', f'{mode}: unexpected descendant output'
        if mode == 'exit':
            harness.until('completed-query')
        assert b'cannot reap query process' not in harness.output
        assert b'cannot terminate query process group' not in harness.output
    finally:
        os.write(harness.gate, b'1')
        harness.close()
        if lifetime is not None:
            os.close(lifetime)


if __name__ == '__main__':
    for mode in sys.argv[1:] or ('cancel', 'regroup', 'exit', 'error', 'overflow', 'stderr-overflow'):
        exercise(mode)
    print('PASS: structured query cancellation, early exit and overflow clean owned children')
