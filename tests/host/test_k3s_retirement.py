#!/usr/bin/env python3
"""Fault-inject the embedded payload without root, a VM, or user state."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('retirement', 'host/migration/retire_k3s.py')
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)


class Retirement(unittest.TestCase):
    def test_each_stage_resumes_without_repeating_committed_steps(self):
        for failed_stage in range(1, len(r.STAGES)):
            with self.subTest(stage=failed_stage), tempfile.TemporaryDirectory() as directory:
                journal = Path(directory) / 'journal'
                unit = Path(directory) / 'missing-unit'
                calls = []
                def action(index):
                    def invoke(*args):
                        calls.append(index)
                        if index == failed_stage:
                            raise RuntimeError('injected failure')
                    return invoke
                with patch.object(r, 'JOURNAL', journal), patch.object(r, 'UNIT', unit), \
                     patch.object(r, 'DATA', ()), patch.object(r, 'FILES', ()), \
                     patch.object(r, 'safe', side_effect=Path), patch.object(r.os, 'geteuid', return_value=0), \
                     patch.object(r, 'stop', side_effect=action(1)), \
                     patch.object(r, 'resources', side_effect=action(2)), \
                     patch.object(r, 'remove_data', side_effect=action(3)), \
                     patch.object(r, 'replace_helpers', side_effect=action(4)), \
                     patch.object(r, 'run', side_effect=action(5)):
                    with self.assertRaises(RuntimeError):
                        r.migrate({})
                    self.assertEqual(json.loads(journal.read_bytes())['stage'], r.STAGES[failed_stage - 1])
                    failed_stage_saved = failed_stage
                    failed_stage = -1
                    calls.clear()
                    r.migrate({})
                    self.assertEqual(calls, list(range(failed_stage_saved, len(r.STAGES))))
                    calls.clear()
                    r.migrate({})
                    self.assertEqual(calls, [])

    def test_containerd_scope_and_snapshot_dependency_order(self):
        commands = []
        listings = 0
        def invoke(*args, **kwargs):
            nonlocal listings
            commands.append(args)
            if args[:2] == ('snapshots', 'list'):
                return subprocess.CompletedProcess(args, 0, b'KEY PARENT KIND\nparent Committed\nchild parent Active\n')
            if args[:2] == ('snapshots', 'remove') and args[-1] == 'parent':
                listings += 1
                return subprocess.CompletedProcess(args, int(listings == 1), b'')
            return subprocess.CompletedProcess(args, 0, b'')
        with patch.object(r, 'ctr', side_effect=invoke):
            r.resources()
        self.assertEqual([c[-1] for c in commands if c[:2] == ('snapshots', 'remove')],
                         ['parent', 'child', 'parent'])
        with patch.object(r, 'run') as run:
            r.ctr('tasks', 'list')
            self.assertIn('k8s.io', run.call_args.args)
            self.assertNotIn('moby', run.call_args.args)

    def test_mounts_and_child_symlinks_preserve_foreign_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            data, outside = root / 'data', root / 'outside'
            data.mkdir()
            outside.write_text('Docker volume sentinel')
            (data / 'link').symlink_to(outside)
            with patch.object(r, 'DATA', (str(data),)), patch.object(r, 'FILES', ()), \
                 patch.object(r, 'LINKS', {}), patch.object(r, 'safe', side_effect=Path):
                with patch.object(Path, 'read_text', return_value=f'1 2 3 4 {data} other'):
                    with self.assertRaises(RuntimeError):
                        r.remove_data()
                    self.assertTrue(data.exists())
                with patch.object(Path, 'read_text', return_value=''):
                    r.remove_data()
            self.assertEqual(outside.read_text(), 'Docker volume sentinel')

    def test_symlink_parent_and_option_identifier_are_rejected(self):
        with self.assertRaises(RuntimeError):
            r.safe('/tmp/hamn-forbidden-path')
        with self.assertRaises(RuntimeError):
            r.identifiers(b'--all\n')
        with self.assertRaises(UnicodeDecodeError):
            r.identifiers(b'\xff')


if __name__ == '__main__':
    unittest.main()
