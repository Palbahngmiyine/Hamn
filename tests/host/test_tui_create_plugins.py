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
        wrapper.write_text(f'#!{sys.executable}\n' + '''import json, os, sys
from pathlib import Path
with Path(os.environ['HOME'], 'native-calls').open('a') as out:
    out.write(json.dumps(sys.argv[1:]) + '\\n')
''' + f'os.execv({kubectl!r},[{kubectl!r}]+sys.argv[1:])\n')
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
        harness.send(b'\r', '[Kubernetes]')
        config_before = (root / 'kubeconfig').read_bytes()
        for value in ('--namespace=value', '--context=value', '--kubeconfig=value'):
            for joined in (False, True):
                args = ['create', 'configmap', 'literal-fixture'] + (
                    ['--from-literal=' + value] if joined else ['--from-literal', value]) + [
                    '--dry-run=client', '--validate=false', '-o', 'yaml']
                direct = subprocess.run([kubectl, '--context', 'old-cluster', '--namespace', 'ui-ns'] + args,
                    env=env, capture_output=True, text=True, timeout=10)
                assert direct.returncode == 0 and 'namespace: ui-ns' in direct.stdout, direct
                def creates():
                    return [json.loads(line) for line in (root / 'native-calls').read_text().splitlines()
                            if 'literal-fixture' in json.loads(line)]
                before = len(creates())
                harness.send(b':' + ' '.join(args).encode() + b'\r', 'Exit code 0')
                screen = harness.screen.text()
                assert all(line.strip() in screen for line in direct.stdout.splitlines()), screen
                assert value not in screen.split('apiVersion:')[0], screen
                assert len(creates()) == before + 1 and creates()[-1][-len(args):] == args, creates()
                harness.send(b'\r', '[Kubernetes]')
        assert (root / 'kubeconfig').read_bytes() == config_before
        for option in ('--dry-run', '--validate'):
            args = ['create', 'configmap', 'optional-fixture', option, '--namespace', 'explicit',
                    '--dry-run=client', '--validate=false', '-o', 'yaml']
            direct = subprocess.run([kubectl, '--context', 'old-cluster', '--namespace', 'ui-ns'] + args,
                env=env, capture_output=True, text=True, timeout=10)
            assert direct.returncode == 0 and 'namespace: explicit' in direct.stdout, direct
            harness.send(b':' + ' '.join(args).encode() + b'\r', 'Exit code 0')
            assert all(line.strip() in harness.screen.text() for line in direct.stdout.splitlines()), harness.screen.text()
            assert '--namespace explicit' in harness.screen.text().split('apiVersion:')[0], harness.screen.text()
            harness.send(b'\r', '[Kubernetes]')
        # A config value that resembles --kubeconfig must not redirect the
        # post-command reload to another file. Both edits own disposable files.
        config = root / 'kubeconfig'
        direct_config = root / 'direct-kubeconfig'
        direct_config.write_bytes(config.read_bytes())
        args = ['config', 'set-credentials', 'literal-user', '--exec-command=/not-executed',
                '--exec-arg', '--kubeconfig=literal']
        direct = subprocess.run([kubectl] + args, env=dict(env, KUBECONFIG=str(direct_config)),
                                capture_output=True, text=True, timeout=10)
        assert direct.returncode == 0, direct.stderr
        harness.send(b':' + ' '.join(args).encode() + b'\r', 'Exit code 0')
        assert direct.stdout.strip() in harness.screen.text(), harness.screen.text()
        harness.send(b'\r', 'Namespace: test')
        assert 'Context: old-cluster' in harness.screen.text(), harness.screen.text()
        assert config.read_bytes() == direct_config.read_bytes()
        assert sorted(p.name for p in (root / '.hamn').iterdir()) == ['tui.json']
        print('PASS: create plugins retain exact argv/exit/count; built-ins, aliases and literal data keep UI namespace')
    finally:
        harness.close()


if __name__ == '__main__':
    main()
