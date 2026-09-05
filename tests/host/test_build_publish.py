#!/usr/bin/env python3
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('build_host', 'scripts/build-host.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class Publication(unittest.TestCase):
    def test_each_failed_gate_preserves_previous_executable_and_removes_candidate(self):
        for failure in ['cargo', 'codesign', 'bash', 'version', 'replace', None]:
            with self.subTest(gate=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source = root / 'target/release/hamn'
                source.parent.mkdir(parents=True)
                source.write_bytes(b'new executable')
                output = root / 'build/hamn'
                output.parent.mkdir()
                output.write_bytes(b'previous signed executable')
                calls = []
                def run(command, **kwargs):
                    calls.append(command[0])
                    if command[0] == failure:
                        raise subprocess.CalledProcessError(1, command)
                original_replace = os.replace
                def replace(*args):
                    if failure == 'replace':
                        raise OSError('injected publication failure')
                    return original_replace(*args)
                with patch.dict(os.environ, CARGO_TARGET_DIR=str(root / 'target')), \
                     patch.object(builder.subprocess, 'run', side_effect=run), \
                     patch.object(builder.subprocess, 'check_output', return_value='hamn wrong' if failure == 'version' else 'hamn 0.0.1'), \
                     patch.object(builder.os, 'replace', side_effect=replace):
                    if failure:
                        with self.assertRaises((subprocess.CalledProcessError, RuntimeError, OSError)):
                            builder.publish(output, '0.0.1', 'release')
                    else:
                        builder.publish(output, '0.0.1', 'release')
                        self.assertEqual(calls, ['cargo', 'codesign', 'bash'])
                self.assertEqual(output.read_bytes(), b'previous signed executable' if failure else b'new executable')
                self.assertEqual(list(output.parent.glob('.hamn-candidate-*')), [])


if __name__ == '__main__':
    unittest.main()
