#!/usr/bin/env python3
"""Compare routing arity with every public command in the installed kubectl."""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def main():
    kubectl = shutil.which('kubectl')
    if not kubectl:
        print('SKIP: installed kubectl unavailable for public flag inventory')
        return
    with tempfile.TemporaryDirectory(prefix='hamn-kubectl-flags-', dir='/tmp') as directory:
        root = Path(directory)
        env = dict(os.environ, HOME=directory, KUBECONFIG=str(root / 'absent'), LANG='C', LC_ALL='C')
        inventory, queue = {}, [()]
        while queue:
            path = queue.pop(0)
            key = ' '.join(path) or 'root'
            if key in inventory:
                continue
            assert len(inventory) < 512, 'unexpected command inventory growth'
            result = subprocess.run([kubectl, 'help', '--', *path], env=env, capture_output=True, text=True, timeout=10)
            assert result.returncode == 0, (path, result.stderr)
            flags, section = {}, ''
            for line in result.stdout.splitlines():
                if line and not line[0].isspace() and line.endswith(':'):
                    section = line
                if 'Commands' in section or section == 'Subcommands:':
                    child = re.match(r'^  ([a-z][a-z0-9-]*)  +\S', line)
                    if child:
                        queue.append(path + (child[1],))
                option = re.match(r'^    (?:-([^, ]), )?--([a-z][a-z0-9-]*)=(.*):$', line)
                if option:
                    short, name, default = option.groups()
                    # Kubectl's public help prints bool defaults without quotes.
                    # These three string flags also have NoOptDefVal: see the
                    # github.com/kubernetes/kubectl/blob/master/pkg/cmd/util/helpers.go
                    # github.com/kubernetes/kubectl/blob/master/pkg/cmd/delete/delete_flags.go
                    consumes = default not in ('true', 'false') and name not in ('cascade', 'dry-run', 'validate')
                    flags['--' + name] = consumes
                    if short:
                        flags['-' + short] = consumes
            inventory[key] = flags
        result = subprocess.run([kubectl, 'version', '--client', '-o', 'json'], env=env,
                                capture_output=True, text=True, check=True, timeout=10)
        version = json.loads(result.stdout)['clientVersion']['gitVersion']
        # `help options` suppresses the body; obtain inherited flags explicitly.
        result = subprocess.run([kubectl, 'options'], env=env, capture_output=True, text=True, check=True, timeout=10)
        inventory['global'] = {}
        for short, name, default in re.findall(r'^    (?:-([^, ]), )?--([a-z][a-z0-9-]*)=(.*):$', result.stdout, re.MULTILINE):
            inventory['global']['--' + name] = default not in ('true', 'false')
            if short:
                inventory['global']['-' + short] = default not in ('true', 'false')
        assert len(inventory) > 50 and inventory['create configmap']['--from-literal']
        assert inventory['logs']['-f'] is False and inventory['get']['-f'] is True
        fixture = root / 'inventory.json'
        fixture.write_text(json.dumps(inventory))
        env = dict(os.environ, HAMN_KUBECTL_FLAG_INVENTORY=str(fixture))
        result = subprocess.run(['cargo', 'test', '--locked', 'native_flags::tests::installed_kubectl_flag_inventory',
                                 '--', '--ignored', '--exact'], env=env, capture_output=True, text=True, timeout=120)
        assert result.returncode == 0 and '1 passed' in result.stdout, result.stdout + result.stderr
        print(f'PASS: {version} public arities and flag-like values across {len(inventory)} command/global entries')


if __name__ == '__main__':
    main()
