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
