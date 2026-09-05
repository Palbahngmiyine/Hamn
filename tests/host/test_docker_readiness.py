#!/usr/bin/env python3
"""A forwarded socket can accept locally while guest access fails."""
from pathlib import Path
import socket
import subprocess
import tempfile
import threading

binary = Path('build/tests/test_docker_readiness').resolve()
for body, status, ready in [(b'OK', 200, True), (b'wrong', 200, False),
                            (b'OK', 403, False), (None, 200, False),
                            (b'O' * 4096, 200, False)]:
    with tempfile.TemporaryDirectory(prefix='hamn-ping-', dir='/tmp') as directory:
        server = socket.socket(socket.AF_UNIX)
        server.bind(directory + '/docker.sock')
        server.listen(1)
        server.settimeout(5)
        requests = []
        errors = []
        def serve():
            try:
                with server.accept()[0] as connection:
                    connection.settimeout(5)
                    requests.append(connection.recv(4096))
                    if body is not None:
                        response = f'HTTP/1.1 {status} Test\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n'.encode() + body
                        connection.sendall(response)
            except Exception as error:
                errors.append(error)
        thread = threading.Thread(target=serve)
        thread.start()
        try:
            result = subprocess.run([binary, directory], capture_output=True, timeout=5)
            assert result.returncode == (0 if ready else 1), (status, body, result)
        finally:
            thread.join(timeout=6)
            server.close()
        assert not thread.is_alive() and not errors, errors
        assert requests and requests[0].startswith(b'GET /_ping HTTP/1.1\r\n')
with tempfile.TemporaryDirectory(prefix='hamn-ping-', dir='/tmp') as directory:
    assert subprocess.run([binary, directory], timeout=5).returncode == 1
print('Docker readiness requires an actual Engine API response: passed')
