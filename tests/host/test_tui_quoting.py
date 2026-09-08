#!/usr/bin/env python3
"""Double-quoted templates must reach the installed kubectl unchanged."""
import os
import shutil
import subprocess
from test_tui_native_regressions import Harness


def main():
    kubectl = shutil.which('kubectl')
    if not kubectl:
        print('SKIP: installed kubectl unavailable; quoting has Rust regressions')
        return
    harness = Harness('kubernetes')
    try:
        harness.until('old-target-row')
        wrapper = harness.root / 'bin/kubectl'
        wrapper.write_text(f'#!{os.sys.executable}\nimport os, sys\n'
                           f'os.execv({kubectl!r}, [{kubectl!r}] + sys.argv[1:])\n')
        env = dict(os.environ, HOME=str(harness.root),
                   KUBECONFIG=str(harness.root / 'kubeconfig'))
        args = ['create', 'configmap', 'quoted', '--from-literal=pattern=a\\nb\\tc\\.d',
                '--dry-run=client', '--validate=false', '-o',
                r"jsonpath={.data.pattern}{'\n'}QUOTE_END"]
        direct = subprocess.run([kubectl] + args, env=env, capture_output=True,
                                text=True, timeout=15)
        assert direct.returncode == 0, direct.stderr
        assert direct.stdout == 'a\\nb\\tc\\.d\nQUOTE_END', direct.stdout
        command = (r'kubectl create configmap quoted --from-literal=pattern="a\nb\tc\.d" '
                   r'''--dry-run=client --validate=false -o "jsonpath={.data.pattern}{'\n'}QUOTE_END"''')
        harness.send(b':' + command.encode() + b'\r', 'Exit code 0')
        screen = harness.screen.text()
        for line in direct.stdout.splitlines():
            assert line in screen, (line, screen)
    finally:
        harness.close()
    print('PASS: real kubectl template and literal bytes match direct argv execution')


if __name__ == '__main__':
    main()
