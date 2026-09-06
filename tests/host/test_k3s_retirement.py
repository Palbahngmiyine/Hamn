#!/usr/bin/env python3
"""Fault-inject the embedded payload without root, a VM, or user state."""
import importlib.util
import hashlib
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
    def test_completed_retirement_can_recover_same_contract_without_deleting_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            journal, transactions = root / 'journal', root / 'transactions'
            journal.write_text('{"version":1,"stage":"complete"}')
            entry = transactions / ('b' * 32)
            helpers = entry / 'data/libexec_hamn'
            helpers.mkdir(parents=True)
            payload = {'helpers': {name: '# trusted ' + name for name in r.HELPERS}}
            for name, content in payload['helpers'].items():
                (helpers / name).write_text(content)
            (entry / 'phase').write_text('ready\n')
            metadata = entry / 'meta'
            metadata.mkdir()
            for name in ('hamnd', 'libexec_hamn', 'hamnd_unit', 'etc_hamn', 'containerd_config',
                         'docker_config', 'docker_dropin', 'host_dns_config', 'host_dns_unit',
                         'modules_config', 'sysctl_config', 'cni_bin'):
                (metadata / name).write_text('present' if name == 'libexec_hamn' else 'absent')
            for service in ('hamnd', 'containerd', 'docker', 'hamn-host-dns'):
                (metadata / f'{service}.service.active').write_text('inactive')
                (metadata / f'{service}.service.enabled').write_text('disabled')
            sentinel = root / 'docker-volume'
            sentinel.write_text('preserve')
            identity = {'version': 1, 'retirement': json.loads(journal.read_text()),
                        'helpers': {name: hashlib.sha256(content.encode()).hexdigest()
                                    for name, content in payload['helpers'].items()}}
            with patch.object(r, 'TRANSACTIONS', transactions), patch.object(r, 'JOURNAL', journal), \
                 patch.object(r, 'safe', side_effect=lambda p: helpers / 'guest-deployment-transaction' if str(p) == '/usr/local/libexec/hamn/guest-deployment-transaction' else Path(p)), patch.object(r, 'run') as run:
                (entry / 'provenance.json').write_text(json.dumps(identity))
                r.recovery_identity(entry, payload)
                for bad in ('different-contract', 'unfinished-retirement', 'retired-file', 'symlink', 'bad-metadata', 'missing-data', 'multiple'):
                    with self.subTest(bad=bad):
                        if bad == 'different-contract':
                            (helpers / r.HELPERS[0]).write_text('altered')
                        elif bad == 'unfinished-retirement':
                            journal.write_text('{"version":1,"stage":"helpers"}')
                        elif bad == 'retired-file':
                            (helpers / 'configure-k3s').write_text('old')
                        elif bad == 'symlink':
                            (helpers / 'outside').symlink_to(sentinel)
                        elif bad == 'bad-metadata':
                            (metadata / 'docker_config').write_text('corrupt')
                        elif bad == 'missing-data':
                            (metadata / 'hamnd').write_text('present')
                        else:
                            (transactions / ('c' * 32)).mkdir()
                        with self.assertRaises(RuntimeError):
                            r.recover_deployment(payload)
                        run.assert_not_called()
                        self.assertTrue(entry.exists())
                        (metadata / 'docker_config').write_text('absent')
                        (metadata / 'hamnd').write_text('absent')
                        extra = transactions / ('c' * 32)
                        if extra.exists(): extra.rmdir()
                        (helpers / r.HELPERS[0]).write_text(payload['helpers'][r.HELPERS[0]])
                        journal.write_text(json.dumps(identity['retirement']))
                        for name in ('configure-k3s', 'outside'):
                            (helpers / name).unlink(missing_ok=True)
                # A verified legacy backup (without provenance) is also safe.
                (entry / 'provenance.json').unlink()
                r.recovery_identity(entry, payload)
                def recovered(*args):
                    import shutil
                    self.assertEqual(args[-2:], ('rollback', 'b' * 32))
                    shutil.rmtree(entry)
                run.side_effect = recovered
                r.recover_deployment(payload)
                r.recover_deployment(payload)
                self.assertEqual(run.call_count, 1)
                self.assertEqual(sentinel.read_text(), 'preserve')

    def test_embedded_helper_allowlist_includes_only_the_migration_contract(self):
        helpers = {name: '# fixed ' + name for name in r.HELPERS}
        self.assertEqual(set(helpers), {'verify-image-contract', 'guest-deployment-transaction', 'configure-docker'})
        with patch.object(r, 'atomic') as write, patch.object(r, 'run') as run:
            r.replace_helpers({'helpers': helpers})
            self.assertEqual(write.call_count, 3)
            run.assert_called_once_with('usermod', '--append', '--groups', 'docker', 'hamn')
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
                    self.assertEqual(calls, [4, 5])  # refresh trusted helpers, then readiness

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
        with patch.object(r, 'ctr', side_effect=invoke), patch.object(r, 'retire_pods'):
            r.resources()
        self.assertEqual([c[-1] for c in commands if c[:2] == ('snapshots', 'remove')],
                         ['parent', 'child', 'parent'])
        with patch.object(r, 'run') as run:
            r.ctr('tasks', 'list')
            self.assertIn('k8s.io', run.call_args.args)
            self.assertNotIn('moby', run.call_args.args)

    def test_snapshot_disappearing_during_containerd_gc_is_already_retired(self):
        listings = iter([b'KEY PARENT KIND\nremoved Committed\n', b'KEY PARENT KIND\n'])
        def invoke(*args, **kwargs):
            if args[:2] == ('snapshots', 'list'):
                return subprocess.CompletedProcess(args, 0, next(listings))
            return subprocess.CompletedProcess(args, int(args[:2] == ('snapshots', 'remove')), b'')
        with patch.object(r, 'ctr', side_effect=invoke), patch.object(r, 'retire_pods'):
            r.resources()

    def test_cri_cleanup_persists_only_valid_pod_uids_and_preserves_backing_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inventory = root / 'inventory'
            pod_root = root / 'pod'
            pod_root.mkdir()
            outside = root / 'volume-data'
            outside.write_text('preserved')
            (pod_root / 'volume-link').symlink_to(outside)
            uid = '12345678-abcd-abcd-abcd-123456789abc'
            pod = {'id': 'a' * 64, 'metadata': {'uid': uid}}
            replies = iter([{'items': [pod]}, {'items': []}])
            commands = []
            def run(*args, **kwargs):
                commands.append(args)
                output = json.dumps(next(replies)).encode() if 'pods' in args else b''
                return subprocess.CompletedProcess(args, 0, output)
            def safe(path):
                if str(path).startswith('/var/lib/kubelet/pods/'):
                    self.assertEqual(str(path).split('/')[-1], uid)
                    return pod_root
                if str(path) == '/usr/local/bin/k3s':
                    return outside
                return Path(path)
            with patch.object(r, 'PODS', inventory), patch.object(r, 'safe', side_effect=safe), \
                    patch.object(r, 'mountpoints', return_value=[]), \
                    patch.object(r, 'run', side_effect=run):
                r.retire_pods()
                self.assertEqual(json.loads(inventory.read_bytes()), [uid])
                self.assertFalse(pod_root.exists())
                self.assertEqual(outside.read_text(), 'preserved')
                self.assertTrue(any('stopp' in command for command in commands))
                self.assertTrue(any('rmp' in command for command in commands))
                self.assertTrue(all('unix:///run/containerd/containerd.sock' in command for command in commands))
                inventory.write_text('["../../docker"]')
                replies = iter([{'items': []}])
                commands.clear()
                with self.assertRaises(RuntimeError):
                    r.retire_pods()
                self.assertFalse(any('rmp' in command for command in commands))

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

    def test_foreign_mount_in_later_directory_prevents_all_data_removal(self):
        with tempfile.TemporaryDirectory() as directory:
            roots = [Path(directory) / name for name in ['k3s', 'kubelet']]
            for root in roots:
                root.mkdir()
                (root / 'sentinel').write_text('preserved')
            with patch.object(r, 'DATA', tuple(map(str, roots))), \
                    patch.object(r, 'safe', side_effect=Path), \
                    patch.object(r, 'mountpoints', return_value=[str(roots[1] / 'foreign')]):
                with self.assertRaises(RuntimeError):
                    r.remove_data()
            for root in roots:
                self.assertEqual((root / 'sentinel').read_text(), 'preserved')

    def test_pod_unmount_failure_preserves_directory_and_retry_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            uid = '12345678-abcd-abcd-abcd-123456789abc'
            inventory = root / 'inventory'
            inventory.write_text(json.dumps([uid]))
            pod = root / 'pod'
            volume = pod / 'volume'
            volume.mkdir(parents=True)
            sentinel = volume / 'sentinel'
            sentinel.write_text('preserved')
            def safe(path):
                return pod if str(path).startswith('/var/lib/kubelet/pods/') else \
                    inventory if str(path) == '/usr/local/bin/k3s' else Path(path)
            def run(*args, **kwargs):
                if args[0] == 'umount':
                    self.assertEqual(args, ('umount', '--', str(volume)))
                    raise RuntimeError('busy mount')
                return subprocess.CompletedProcess(args, 0, b'{"items":[]}')
            with patch.object(r, 'PODS', inventory), patch.object(r, 'safe', side_effect=safe), \
                    patch.object(r, 'mountpoints', return_value=[str(volume), '/run/docker/netns/foreign']), \
                    patch.object(r, 'run', side_effect=run):
                with self.assertRaisesRegex(RuntimeError, 'busy mount'):
                    r.retire_pods()
            self.assertEqual(sentinel.read_text(), 'preserved')
            self.assertEqual(json.loads(inventory.read_bytes()), [uid])

    def test_symlink_parent_and_option_identifier_are_rejected(self):
        with self.assertRaises(RuntimeError):
            r.safe('/tmp/hamn-forbidden-path')
        with self.assertRaises(RuntimeError):
            r.identifiers(b'--all\n')
        with self.assertRaises(UnicodeDecodeError):
            r.identifiers(b'\xff')


if __name__ == '__main__':
    unittest.main()
