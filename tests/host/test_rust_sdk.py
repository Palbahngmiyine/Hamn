#!/usr/bin/env python3
"""The build script must propagate the selected Apple SDK to the final linker."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class RustSdk(unittest.TestCase):
    def test_selected_sdk_is_a_separate_linker_argument(self):
        with tempfile.TemporaryDirectory(prefix='hamn-sdk-') as directory:
            root = Path(directory)
            script = root / 'build-script'
            subprocess.run(['rustc', 'build.rs', '-o', script], check=True)
            sdk = root / 'Apple SDK with spaces'
            sdk.mkdir()
            runtime = root / 'libclang_rt.osx.a'
            runtime.touch()
            # Only stub native compilation: execute the real Rust build script.
            for name, content in [('make', '#!/bin/sh\nprintf "%s\\n" "$@" > "$TEST_MAKE_ARGS"\n'),
                                  ('clang', '#!/bin/sh\nprintf "%s\\n" "$TEST_RUNTIME"\n')]:
                path = root / name
                path.write_text(content)
                path.chmod(0o700)
            env = dict(os.environ, PATH=str(root) + ':' + os.environ['PATH'],
                       CARGO_CFG_TARGET_OS='macos', CARGO_MANIFEST_DIR=str(Path.cwd()),
                       OUT_DIR=str(root / 'out'), TEST_RUNTIME=str(runtime),
                       TEST_MAKE_ARGS=str(root / 'make-args'))
            cases = [(value, None) for value in
                     [str(sdk), '', None, str(root / 'missing'), 'relative-sdk']]
            # A toolchain wrapper may reset SDKROOT; the runner's explicit SDK wins.
            cases += [(str(root / 'wrong-nix-sdk'), str(sdk)),
                      (None, str(sdk)), (str(sdk), ''), (str(sdk), str(root / 'missing'))]
            for selected, preserved in cases:
                effective = preserved or selected
                with self.subTest(sdk=selected, preserved=preserved):
                    current = dict(env)
                    current.pop('SDKROOT', None)
                    current.pop('HAMN_SYSTEM_SDKROOT', None)
                    if selected is not None:
                        current['SDKROOT'] = selected
                    if preserved is not None:
                        current['HAMN_SYSTEM_SDKROOT'] = preserved
                    result = subprocess.run([script], env=current, text=True, capture_output=True)
                    if effective in (str(root / 'missing'), 'relative-sdk'):
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn('SDKROOT must name', result.stderr)
                    else:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        args = [line.removeprefix('cargo:rustc-link-arg=')
                                for line in result.stdout.splitlines()
                                if line.startswith('cargo:rustc-link-arg=')]
                        self.assertEqual(args, ['-isysroot', str(sdk)] if effective else [])
                        native_args = (root / 'make-args').read_text().splitlines()
                        self.assertEqual([arg for arg in native_args if arg.startswith('SDKROOT=')],
                                         ['SDKROOT=' + str(sdk)] if effective else [])


if __name__ == '__main__':
    unittest.main()
