#!/usr/bin/env python3
"""Exercise the embedded Docker client against a local HTTP Unix socket."""
import json
import os
from pathlib import Path
import re
import socketserver
import subprocess
import tempfile
import threading

binary = Path(os.environ.get("HAMN", "target/debug/hamn")).resolve()
requests = []
mode = {"status": 200}


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        line = self.rfile.readline().decode().strip()
        method, path, _ = line.split(" ")
        requests.append((method, path))
        while self.rfile.readline().strip():
            pass
        path = re.sub(r"^/v[0-9.]+", "", path).split("?")[0]
        status = mode["status"]
        if path == "/version":
            data, status = {"ApiVersion": "1.53", "MinAPIVersion": "1.40"}, 200
        elif path == "/containers/json":
            data = [{"Id": "abc123", "Names": ["/sample"], "State": "running"}]
        elif path.endswith("/json"):
            data = {"Id": "abc123", "Name": "/sample", "State": {"Running": True}}
        else:
            data, status = {}, 204
        if status != 200 and status != 204:
            data = {"message": "fixture denied"}
        body = b"" if status == 204 else json.dumps(data).encode()
        self.wfile.write(f"HTTP/1.1 {status} Fixture\r\nContent-Length: {len(body)}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n".encode() + body)


with tempfile.TemporaryDirectory(prefix="hamn-docker-") as directory:
    env = dict(os.environ, HOME=directory, PATH="/usr/bin:/bin")

    def run(*args):
        result = subprocess.run([binary, "--headless", *args], env=env,
                                capture_output=True, text=True, timeout=15)
        return result.returncode, json.loads(result.stdout)

    assert run("vm", "create", "--profile", "test", "--yes")[0] == 0
    socket = str(Path(directory) / ".hamn/test/docker.sock")
    with socketserver.UnixStreamServer(socket, Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            rc, result = run("docker", "containers", "list", "--profile", "test")
            assert rc == 0 and result["data"][0]["Id"] == "abc123", result
            rc, result = run("docker", "containers", "start", "sample", "--profile", "test", "--yes")
            assert rc == 0, result
            assert any(method == "POST" and "/containers/abc123/start" in path for method, path in requests)
            before = len(requests)
            assert run("docker", "containers", "delete", "sample", "--profile", "test")[0] != 0
            assert len(requests) == before
            mode["status"] = 403
            rc, result = run("docker", "containers", "list", "--profile", "test")
            assert rc != 0 and result["error"]["code"] == "permissionDenied", result
        finally:
            server.shutdown()
            thread.join(timeout=5)
            assert not thread.is_alive()
print("Docker API without Docker CLI: passed")
