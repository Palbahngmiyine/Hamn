#!/usr/bin/env python3
"""Installed kubectl dispatches create extensions once, preserving plugin argv."""
import json
import os
import shutil
import subprocess
import sys
from test_tui_native_regressions import Harness


def main():
    kubectl = shutil.which('kubectl')
    if not kubectl:
        print('SKIP: installed kubectl unavailable for create plugin dispatch')
        return
    harness = Harness('kubernetes', namespace='ui-ns')
    try:
        harness.until('old-target-row')
        root = harness.root
        plugin = root / 'bin/kubectl-create-hamnfixture'
        plugin.write_text(f'#!{sys.executable}\n' + '''import json, os, sys
from pathlib import Path
with Path(os.environ['HOME'], 'plugin-calls').open('a') as out:
    out.write(json.dumps(sys.argv[1:]) + '\\n')
print('PLUGIN_ARGV=' + json.dumps(sys.argv[1:]), flush=True)
sys.exit(7)
''')
        plugin.chmod(0o700)
        # These installed names must never displace a built-in command or alias.
        builtins = 'clusterrole clusterrolebinding configmap cm cronjob cj deployment deploy ingress ing job namespace ns poddisruptionbudget pdb priorityclass pc quota resourcequota role rolebinding secret service svc serviceaccount sa token'.split()
        for name in [''] + ['-' + name for name in builtins]:
            shadow = root / ('bin/kubectl-create' + name)
            shadow.write_text('#!/bin/sh\nprintf UNEXPECTED_SHADOW_PLUGIN\nexit 99\n')
            shadow.chmod(0o700)
        wrapper = root / 'bin/kubectl'
        wrapper.write_text(f'#!{sys.executable}\nimport os,sys\nos.execv({kubectl!r},[{kubectl!r}]+sys.argv[1:])\n')
        env = dict(os.environ, HOME=str(root), KUBECONFIG=str(root / 'kubeconfig'),
            PATH=f'{root}/bin:/usr/bin:/bin')
        for prefix in ('', 'kubectl '):
            args = ['create', 'hamnfixture', 'marker', '--plugin-option', 'value']
            direct = subprocess.run([kubectl] + args, env=env, capture_output=True,
                text=True, timeout=10)
            assert direct.returncode == 7, direct
            before = len((root / 'plugin-calls').read_text().splitlines())
            harness.send(b':' + (prefix + ' '.join(args)).encode() + b'\r', 'Exit code 7')
            assert direct.stdout.strip() in harness.screen.text(), harness.screen.text()
            calls = (root / 'plugin-calls').read_text().splitlines()
            assert len(calls) == before + 1 and json.loads(calls[-1]) == args[2:], calls
            harness.send(b'\r', '[Kubernetes]')

        for name in builtins:
            args = ['create', name, '--help']
            direct = subprocess.run([kubectl] + args, env=env, capture_output=True,
                text=True, timeout=10)
            assert direct.returncode == 0 and 'UNEXPECTED_SHADOW_PLUGIN' not in direct.stdout
            harness.send(b':' + ' '.join(args).encode() + b'\r', 'Exit code 0')
            screen = harness.screen.text()
            assert 'UNEXPECTED_SHADOW_PLUGIN' not in screen and 'Plugin-defined' not in screen, screen
            harness.send(b'\r', '[Kubernetes]')
        # The ordinary built-in still uses UI namespace defaults. This dry run
        # creates only stdout and cannot create a Kubernetes resource.
        harness.send(b':create configmap example --dry-run=client --validate=false -o yaml\r', 'Exit code 0')
        assert 'namespace: ui-ns' in harness.screen.text(), harness.screen.text()
        assert sorted(p.name for p in (root / '.hamn').iterdir()) == ['tui.json']
        print('PASS: create plugins retain exact argv/exit/count; built-ins and aliases keep precedence and UI namespace')
    finally:
        harness.close()


if __name__ == '__main__':
    main()
