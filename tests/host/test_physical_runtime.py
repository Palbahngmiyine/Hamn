#!/usr/bin/env python3
import importlib.util
import io
import json
import shutil
import contextlib
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path('packaging/release').resolve()))
from physical_runtime import Runtime
spec = importlib.util.spec_from_file_location('physical', 'packaging/release/physical-e2e.py')
physical = importlib.util.module_from_spec(spec)
spec.loader.exec_module(physical)


class PhysicalRuntime(unittest.TestCase):
    def terminal_candidate(self, directory, change=None):
        """Real PTY/CLI fixture: entry stays read-only unless a fault is injected."""
        root = Path(directory)
        (root / 'state.json').write_text(json.dumps({'state': 'running', 'migration': 'pending', 'lastOperation': None}))
        (root / 'change.json').write_text(json.dumps(change))
        binary = root / 'terminal-fixture'
        binary.write_text('#!' + sys.executable + '\n' + '''
import json, os, pathlib, sys, termios, tty
root = pathlib.Path(os.environ['HOME'])
args = sys.argv[1:]
with (root / 'requests.jsonl').open('a') as output: output.write(json.dumps(args) + '\\n')
state = json.loads((root / 'state.json').read_text())
if args:
    assert args in [
        ['--headless', 'vm', 'status', '--profile', 'legacy'],
        ['--headless', 'vm', 'migrate', '--profile', 'legacy', '--yes']]
    if args[2] == 'migrate':
        state['migration'] = 'current'
        (root / 'state.json').write_text(json.dumps(state))
    print(json.dumps({'schemaVersion': 1, 'ok': True, 'data': state}))
else:
    before = termios.tcgetattr(0)
    try:
        tty.setraw(0)
        os.write(1, b'Hamn')
        assert os.read(0, 1) == b'q'
        change = json.loads((root / 'change.json').read_text())
        if change:
            if change[1] == 'missing': state.pop(change[0])
            else: state[change[0]] = change[1]
            (root / 'state.json').write_text(json.dumps(state))
    finally: termios.tcsetattr(0, termios.TCSANOW, before)
''')
        binary.chmod(0o700)
        return Runtime(binary, root, 'docker')

    def test_running_retirement_opens_read_only_tui_then_explicitly_confirms_migration(self):
        with tempfile.TemporaryDirectory() as directory:
            runtime = self.terminal_candidate(directory)
            with patch.object(runtime, 'ssh', return_value='') as ssh:
                runtime.retire_running('legacy')
            script = ssh.call_args.args[0]
            for required in ['timeout 190', 'systemctl is-active', 'systemctl is-enabled', '--request-timeout=5s', '/readyz']:
                self.assertIn(required, script)
            self.assertEqual(ssh.call_args.kwargs, {'profile': 'legacy'})
            requests = [json.loads(line) for line in (Path(directory) / 'requests.jsonl').read_text().splitlines()]
            self.assertEqual(requests, [
                ['--headless', 'vm', 'status', '--profile', 'legacy'], [],
                ['--headless', 'vm', 'status', '--profile', 'legacy'],
                ['--headless', 'vm', 'migrate', '--profile', 'legacy', '--yes']])

    def test_running_retirement_requires_guest_readiness_before_confirming(self):
        with tempfile.TemporaryDirectory() as directory:
            runtime = self.terminal_candidate(directory)
            with patch.object(runtime, 'ssh', side_effect=RuntimeError('guest K3s not ready')):
                with self.assertRaisesRegex(RuntimeError, 'not ready'):
                    runtime.retire_running('legacy')
            self.assertNotIn('"migrate"', (Path(directory) / 'requests.jsonl').read_text())

    def test_legacy_environment_preserves_host_tool_path_but_isolates_targets(self):
        with patch.dict('os.environ', {'PATH': '/owned/docker:/owned/kubectl:/usr/bin',
                        'HOME': '/unrelated/home', 'DOCKER_HOST': 'tcp://unrelated:2375',
                        'DOCKER_CONTEXT': 'unrelated', 'DOCKER_CONFIG': '/unrelated/docker',
                        'KUBECONFIG': '/unrelated/kube'}, clear=True):
            env = physical.legacy_environment(Path('/private/tmp/owned/home'))
        self.assertEqual(env, {'HOME': '/private/tmp/owned/home', 'PATH': '/owned/docker:/owned/kubectl:/usr/bin',
                              'LC_ALL': 'C', 'DOCKER_CONFIG': '/private/tmp/owned/home/.docker',
                              'KUBECONFIG': '/private/tmp/owned/home/.kube/config'})

    def test_running_retirement_rejects_tui_changes_before_any_confirmed_mutation(self):
        for change in [('state', 'stopped'), ('migration', 'current'), ('lastOperation', {'operation': 'vm migrate'}),
                       ('state', 'missing'), ('migration', 'missing'), ('lastOperation', 'missing')]:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                with self.assertRaisesRegex(RuntimeError, 'before confirmation'):
                    self.terminal_candidate(directory, change).retire_running('legacy')
                requests = (Path(directory) / 'requests.jsonl').read_text()
                self.assertNotIn('"migrate"', requests)
                self.assertNotIn('"--yes"', requests)

    def test_running_retirement_rejects_invalid_before_state_without_opening_tui(self):
        for before in [{}, {'state': 'running', 'migration': 'pending'},
                       {'state': 'stopped', 'migration': 'pending', 'lastOperation': None},
                       {'state': 'running', 'migration': 'current', 'lastOperation': None}]:
            runtime = Runtime('/tmp/candidate', '/tmp/isolated', 'docker')
            with patch.object(runtime, 'call', return_value=before) as call, patch.object(runtime, 'terminal') as terminal:
                with self.assertRaisesRegex(RuntimeError, 'pending retirement'):
                    runtime.retire_running('legacy')
                terminal.assert_not_called()
                call.assert_called_once_with('vm', 'status', profile='legacy')

    def make_legacy_fixture(self, work, state, config=None):
        source = work / 'source'
        source.mkdir()
        names = ['disk.img', 'id_ed25519', 'id_ed25519.pub', 'efi-vars.bin', 'machine-id.bin', 'mac-addr']
        for name in names:
            (source / name).write_bytes(name.encode())
        (source / 'config.yaml').write_text(config or 'mountHome: false\nmounts: []\nprovision: []\nkubernetes:\n  enabled: true\n')
        (source / 'expected.json').write_text(json.dumps({'k3sState': state, 'docker': {'sentinel': 'independent-before'}}))
        home = work / 'home'
        home.mkdir(mode=0o700)
        (home / '.hamn').mkdir(mode=0o700)
        return source, home

    def test_only_running_clone_shares_private_home_without_changing_source_or_oracle(self):
        for state in ['running', 'stopped']:
            work = physical.workspace()
            try:
                source, home = self.make_legacy_fixture(work, state)
                original = {path.name: path.read_bytes() for path in source.iterdir()}
                clone = home / '.hamn/legacy'
                expected, digest = physical.fixture(source, clone, state, 'a' * 64,
                                                    home=home if state == 'running' else None)
                self.assertEqual(expected, {'sentinel': 'independent-before'})
                self.assertEqual(len(digest), 64)
                self.assertIn('mountHome: ' + ('true' if state == 'running' else 'false'), (clone / 'config.yaml').read_text())
                self.assertEqual({path.name: path.read_bytes() for path in source.iterdir()}, original)
                self.assertEqual(set(path.name for path in clone.iterdir()), set(original) - {'expected.json'})
                self.assertFalse((clone / 'seed.iso').exists())
                self.assertFalse((clone / 'guest-deployment.version').exists())
            finally:
                shutil.rmtree(work)

    def test_legacy_home_sharing_rejects_symlinks_permissions_outside_clone_and_stopped_case(self):
        for fault in ['home-link', 'shared-mode', 'parent-mode', 'outside-clone', 'hamn-link', 'stopped']:
            work = physical.workspace()
            try:
                source, home = self.make_legacy_fixture(work, 'running')
                clone = home / '.hamn/legacy'
                if fault == 'home-link':
                    link = work / 'home-link'; link.symlink_to(home, target_is_directory=True); home = link
                elif fault == 'shared-mode': home.chmod(0o755)
                elif fault == 'parent-mode': work.chmod(0o755)
                elif fault == 'outside-clone': clone = work / 'outside'
                elif fault == 'hamn-link':
                    (home / '.hamn').rmdir(); (work / 'elsewhere').mkdir(); (home / '.hamn').symlink_to(work / 'elsewhere', target_is_directory=True)
                with self.assertRaises(ValueError):
                    physical.fixture(source, clone, 'stopped' if fault == 'stopped' else 'running', 'a' * 64, home=home)
                self.assertFalse(clone.exists())
            finally:
                shutil.rmtree(work)

    def test_legacy_home_sharing_rejects_non_temporary_home_and_ambiguous_boolean(self):
        for outside in [Path('/private/tmp'), Path('/')]:
            with self.assertRaises(ValueError):
                physical.fixture(outside / 'source', outside / '.hamn/legacy', 'running', 'a' * 64, home=outside)
        for value in ['', 'mountHome: maybe\n', 'mountHome: false\nmountHome: true\n']:
            work = physical.workspace()
            try:
                source, home = self.make_legacy_fixture(work, 'running', value + 'mounts: []\nprovision: []\nkubernetes:\n')
                with self.assertRaisesRegex(ValueError, 'mountHome boolean'):
                    physical.fixture(source, home / '.hamn/legacy', 'running', 'a' * 64, home=home)
            finally:
                shutil.rmtree(work)

    def test_legacy_fixture_rejects_custom_or_duplicate_mounts_and_provision(self):
        for value in ['mounts: []\nmounts:\n  - location: /outside\nprovision: []\n',
                      '# mounts: []\nmounts:\n  - location: /outside\nprovision: []\n',
                      'mounts: []\nprovision: []\nprovision:\n  - script: echo unexpected\n']:
            work = physical.workspace()
            try:
                source, home = self.make_legacy_fixture(work, 'running', 'mountHome: true\n' + value + 'kubernetes:\n')
                with self.assertRaisesRegex(ValueError, 'custom mounts or provision'):
                    physical.fixture(source, home / '.hamn/legacy', 'running', 'a' * 64, home=home)
            finally:
                shutil.rmtree(work)

    def test_workspace_keeps_profile_sockets_below_darwin_path_limit(self):
        with patch.dict('os.environ', {'TMPDIR': '/var/folders/' + 'x' * 80}):
            work = physical.workspace()
        try:
            socket = work / 'home/.hamn/retire-stopped/docker.sock'
            self.assertLess(len(str(socket).encode()), 104)
            self.assertEqual(work.stat().st_mode & 0o777, 0o700)
        finally:
            shutil.rmtree(work)

    def test_legacy_config_fixture_never_overwrites_an_existing_home_config(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            physical.legacy_kubeconfig(home)
            path = home / '.kube/config'
            before = path.read_bytes()
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(before)['clusters'][0]['cluster']['server'], 'http://127.0.0.1:9')
            with self.assertRaises(FileExistsError):
                physical.legacy_kubeconfig(home)
            self.assertEqual(path.read_bytes(), before)

    def test_terminal_quit_drains_output_larger_than_the_pty_queue(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'terminal-fixture'
            binary.write_text('#!' + sys.executable + '\n' +
                'import os,termios,tty\n'
                'before=termios.tcgetattr(0)\n'
                'try:\n'
                ' tty.setraw(0)\n'
                ' os.write(1,b"Hamn")\n'
                ' for _ in range(64): os.write(1,b"x"*4096)\n'
                ' assert os.read(0,1)==b"q"\n'
                'finally: termios.tcsetattr(0,termios.TCSANOW,before)\n')
            binary.chmod(0o700)
            Runtime(binary, directory, 'docker').terminal()

    def test_snapshot_preserves_user_network_ids_and_requires_builtin_networks(self):
        runtime = Runtime('/tmp/hamn', '/tmp/home', 'docker')
        networks = [{'Name': name, 'Id': name + '-id'} for name in ['bridge', 'host', 'none', 'user']]
        def call(*words, **kwargs):
            return networks if words[1] == 'networks' else []
        with patch.object(runtime, 'call', side_effect=call), patch.object(runtime, 'engine', return_value='hash /data/sentinel'):
            before = runtime.snapshot('fixture')
            networks[0]['Id'] = 'recreated-bridge'
            self.assertEqual(runtime.snapshot('fixture'), before)
            self.assertEqual(before['networks'], ['user-id'])
            networks[-1]['Id'] = 'changed-user-network'
            self.assertNotEqual(runtime.snapshot('fixture'), before)
            del networks[0]
            with self.assertRaises(RuntimeError):
                runtime.snapshot('fixture')

    def test_invalid_network_mode_is_rejected_before_any_runtime_action(self):
        for value in ['true', '', '2']:
            with patch('sys.argv', ['physical-e2e.sh']), \
                    patch.dict('os.environ', {'HAMN_E2E_K8S_HOST_NETWORK': value}, clear=True), \
                    patch.object(physical, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'HOST_NETWORK'):
                    physical.main()
                run.assert_not_called()

    def test_log_records_are_read_as_ndjson_and_failures_are_not_hidden(self):
        runtime = Runtime('/tmp/candidate/hamn', '/tmp/isolated', '/usr/local/bin/docker')
        records = [{'schemaVersion': 1, 'ok': True, 'data': {'text': 'hello'}},
                   {'schemaVersion': 1, 'ok': True, 'data': {'complete': True}}]
        with patch('physical_runtime.run', return_value='\n'.join(map(json.dumps, records))):
            self.assertEqual(runtime.call('docker', 'containers', 'logs', 'fixture'), {'complete': True})
        records[0]['ok'] = False
        with patch('physical_runtime.run', return_value='\n'.join(map(json.dumps, records))):
            with self.assertRaises(RuntimeError):
                runtime.call('docker', 'containers', 'logs', 'fixture')

    def test_help_needs_no_validator_environment_or_runtime(self):
        output = io.StringIO()
        with patch('sys.argv', ['physical-e2e.sh', '--help']), patch.dict('os.environ', {}, clear=True), \
                patch.object(physical, 'run') as run, contextlib.redirect_stdout(output):
            with self.assertRaises(SystemExit) as result:
                physical.main()
        self.assertEqual(result.exception.code, 0)
        self.assertIn('usage: physical-e2e.sh', output.getvalue())
        run.assert_not_called()

    def test_headless_operations_target_only_the_isolated_profile(self):
        runtime = Runtime('/tmp/candidate/hamn', '/tmp/isolated', '/usr/local/bin/docker')
        with patch('physical_runtime.run', return_value=json.dumps({'schemaVersion': 1, 'ok': True, 'data': {'state': 'stopped'}})) as run:
            runtime.call('vm', 'stop', profile='retire-running', yes=True)
            command, env = run.call_args.args
            self.assertEqual(command[1:], ['--headless', 'vm', 'stop', '--profile', 'retire-running', '--yes'])
            self.assertEqual(env['HOME'], '/tmp/isolated')
            self.assertNotIn('/usr/local/bin', env['PATH'])
        with patch('physical_runtime.run', return_value=json.dumps({'schemaVersion': 1, 'ok': False, 'error': {'code': 'outcomeUnknown'}})):
            with self.assertRaises(RuntimeError):
                runtime.call('vm', 'stop', yes=True)

    def test_candidate_archive_rejects_links_duplicates_and_traversal(self):
        for names, link, valid in [(['root/bin/hamn'], False, True), (['../outside'], False, False),
                                    (['/outside'], False, False), (['root/a', 'root/a'], False, False),
                                    (['root/link'], True, False)]:
            with self.subTest(names=names), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive = root / 'archive.tar'
                with tarfile.open(archive, 'w') as bundle:
                    for name in names:
                        entry = tarfile.TarInfo(name)
                        if link:
                            entry.type = tarfile.SYMTYPE
                            entry.linkname = '/outside'
                            bundle.addfile(entry)
                        else:
                            entry.size = 1
                            bundle.addfile(entry, io.BytesIO(b'x'))
                destination = root / 'unpacked'
                destination.mkdir()
                if valid:
                    self.assertEqual(physical.unpack(archive, destination), destination / 'root')
                else:
                    with self.assertRaises(ValueError):
                        physical.unpack(archive, destination)
                    self.assertEqual(list(destination.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
