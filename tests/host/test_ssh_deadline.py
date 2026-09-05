#!/usr/bin/env python3
"""A real OpenSSH client must not wait forever on an unresponsive master."""
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time

binary = Path(os.environ.get('SSH_DEADLINE_TEST', 'build/tests/test_ssh_deadline')).resolve()
for operation, budget in [('alive', 5), ('exit', 5), ('start', 1),
                           ('forward', 5), ('cancel', 5), ('exec', .2)]:
    with tempfile.TemporaryDirectory(prefix='hamn-ssh-', dir='/tmp') as directory:
        server = socket.socket(socket.AF_UNIX)
        server.bind(directory + '/ssh.sock')
        server.listen(1)
        server.settimeout(10)
        accepted, release = threading.Event(), threading.Event()
        errors = []

        def serve():
            try:
                with server.accept()[0]:
                    accepted.set()
                    assert release.wait(12)
            except Exception as error:
                errors.append(error)

        thread = threading.Thread(target=serve)
        thread.start()
        try:
            started = time.monotonic()
            result = subprocess.run([binary, directory, operation],
                capture_output=True, env=dict(os.environ, PATH='/usr/bin:/bin'), timeout=budget + 4)
            elapsed = time.monotonic() - started
            assert accepted.is_set(), (operation, result)
            assert result.returncode == 0, (operation, result)
            assert budget <= elapsed < budget + 4, (operation, elapsed)
            if operation == 'forward':
                assert result.stdout.count(b'completion observed failure') == 1, result
        finally:
            release.set()
            thread.join(timeout=12)
            server.close()
        assert not errors and not thread.is_alive(), errors
print('PASS: real SSH check/exit/start/forward/cancel/exec obey operation deadlines')
