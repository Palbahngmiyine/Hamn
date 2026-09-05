#!/usr/bin/env python3
import importlib.util
import io
import json
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
