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
    def test_embedded_helper_allowlist_includes_only_the_migration_contract(self):
        helpers = {name: '# fixed ' + name for name in r.HELPERS}
        self.assertEqual(set(helpers), {'verify-image-contract', 'guest-deployment-transaction', 'configure-docker'})
        with patch.object(r, 'atomic') as write:
            r.replace_helpers({'helpers': helpers})
            self.assertEqual(write.call_count, 3)
            for call in write.call_args_list:
                path, content, mode = call.args
                self.assertEqual(path.parent, Path('/usr/local/libexec/hamn'))
                self.assertEqual(content, helpers[path.name].encode())
                self.assertEqual(mode, 0o755)
            for invalid in ({}, dict(helpers, arbitrary='untrusted')):
                write.reset_mock()
                with self.assertRaises(RuntimeError):
                    r.replace_helpers({'helpers': invalid})
                write.assert_not_called()

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
                     patch.object(r, 'recover_deployment'), \
                     patch.object(r, 'stop', side_effect=action(1)), \
                     patch.object(r, 'resources', side_effect=action(2)), \
                     patch.object(r, 'remove_data', side_effect=action(3)), \
                     patch.object(r, 'replace_helpers', side_effect=action(4)), \
                     patch.object(r, 'docker_ready', side_effect=action(5)):
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
                    self.assertEqual(calls, [5])  # readiness is always rechecked

    def test_old_deployment_backup_is_recovered_before_retirement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            transactions = root / 'transactions'
            transactions.mkdir()
            entry = transactions / ('a' * 32)
            entry.mkdir()
            phase = entry / 'phase'
            phase.write_text('incomplete')
            with patch.object(r, 'TRANSACTIONS', transactions), patch.object(r, 'JOURNAL', root / 'journal'), \
                 patch.object(r, 'safe', side_effect=Path), patch.object(r, 'run') as run:
                with self.assertRaises(RuntimeError):
                    r.recover_deployment()
                run.assert_not_called()
                phase.write_text('ready\n')
                with self.assertRaises(RuntimeError):
                    r.recover_deployment()  # reported success without cleanup is rejected
                self.assertEqual(run.call_args.args[-2:], ('rollback', 'a' * 32))
                def recovered(*_args):
                    phase.unlink()
                    entry.rmdir()
                run.side_effect = recovered
                r.recover_deployment()
                r.recover_deployment()
                self.assertFalse(entry.exists())

    def test_docker_success_status_with_wrong_body_is_not_ready(self):
        with patch.object(r, 'run', return_value=subprocess.CompletedProcess([], 0, b'not ready')):
            with self.assertRaises(RuntimeError):
                r.docker_ready()
        with patch.object(r, 'run', return_value=subprocess.CompletedProcess([], 0, b'OK\n')):
            r.docker_ready()

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
