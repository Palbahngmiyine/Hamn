#!/usr/bin/env python3
"""A delayed connection reload must not block TUI input or survive navigation."""
import os
from test_tui_native_regressions import Harness


def exercise(workspace):
    harness = Harness(workspace)
    try:
        harness.until('old-target-row')
        program = 'docker' if workspace == 'containers' else 'kubectl'
        peer = harness.root / 'bin' / program
        source = peer.read_text()
        condition = "if 'ls' in args:" if workspace == 'containers' else "if 'view' in args:"
        source = source.replace(condition, condition + """
        with (root / 'notice').open('w') as notice:
            notice.write('reload-blocked\\n'); notice.flush()
        with (root / 'gate').open() as gate:
            gate.read(1)""")
        peer.write_text(source)
        command = 'docker context show' if workspace == 'containers' else 'kubectl config use-context new-cluster'
        harness.send(b':' + command.encode() + b'\r', 'CONFIG_DONE')
        harness.until('Exit code 0')
        os.write(harness.master, b'\r')
        harness.wait(lambda: b'reload-blocked' in harness.notices)
        harness.send(b':version', ':version')
        harness.send(b'\r', 'ACTION_DONE')
        harness.until('Exit code 0')
        harness.send(b'\r', 'old-target-row')
        # The completed newer invocation is an observable cancellation barrier.
        # Releasing the old response must not apply its external context later.
        os.write(harness.gate, b'1')
        query = b':ps\r' if workspace == 'containers' else b':get pods\r'
        harness.send(query, 'old-target-row')
        target = 'external' if workspace == 'containers' else 'new-cluster'
        assert target not in harness.screen.text(), harness.screen.text()
        assert not any(target in args for _, args in harness.calls() if 'ps' in args or 'get' in args), harness.calls()
    finally:
        os.write(harness.gate, b'1')
        harness.close()


if __name__ == '__main__':
    for workspace in ('containers', 'kubernetes'):
        exercise(workspace)
    print('PASS: delayed target reload remains responsive and navigation cancels it')
