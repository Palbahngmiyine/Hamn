#!/usr/bin/env python3
"""Resuming physical validation must preserve the frozen executable artifact."""
import json
from pathlib import Path
import tempfile
import unittest
from test_workspace_live import REPO, prepare


class Preparation(unittest.TestCase):
    def test_resume_reuses_a_read_only_candidate_and_rejects_changed_bytes(self):
        with tempfile.TemporaryDirectory(prefix='hamn-live-prepare-') as directory:
            root = Path(directory)
            (root / 'home').mkdir()
            (root / 'ownership.json').write_text(json.dumps({
                'workspace': str(REPO), 'profile': 'verify', 'home': str(root / 'home')}))
            source = root / 'candidate'
            source.write_bytes(b'fixed candidate bytes')
            source.chmod(0o755)
            frozen = root / 'hamn-under-test'
            frozen.write_bytes(source.read_bytes())
            frozen.chmod(0o555)
            before = frozen.stat()
            try:
                _, runtime = prepare(source, root / 'unused-cache', root)
                self.assertEqual(runtime.binary, frozen)
                self.assertEqual(frozen.read_bytes(), source.read_bytes())
                self.assertEqual(frozen.stat().st_ino, before.st_ino)
                self.assertEqual(frozen.stat().st_mtime_ns, before.st_mtime_ns)
                self.assertEqual(frozen.stat().st_mode, before.st_mode)
                source.write_bytes(b'different candidate bytes')
                with self.assertRaisesRegex(ValueError, 'different candidate'):
                    prepare(source, root / 'unused-cache', root)
                self.assertEqual(frozen.read_bytes(), b'fixed candidate bytes')
            finally:
                frozen.chmod(0o755)


if __name__ == '__main__':
    unittest.main()
