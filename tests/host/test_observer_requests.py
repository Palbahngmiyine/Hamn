#!/usr/bin/env python3
"""Observe actual HTTP requests from the C snapshot reader, with bounded fixtures."""
import http.server
import json
import os
from pathlib import Path
import socketserver
import subprocess
import sys
import threading
import time


class Server(socketserver.UnixStreamServer):
    pass


def measure(binary, root, count):
    profile = root / f"observer-{count}"
    profile.mkdir()
    socket = profile / "docker.sock"
    calls = []
    rows = [{"Id": f"{i:064x}", "Ports": [{"IP": "127.0.0.1",
        "PrivatePort": 80, "PublicPort": 40000 + i, "Type": "tcp"}]}
        for i in range(count)]
    payload = json.dumps(rows).encode()

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_GET(self):
            calls.append(self.path)
            time.sleep(0.01)  # Deliberate workload latency, not synchronization.
            body = payload if self.path == "/containers/json" else b"{}"
            self.send_response(200 if self.path == "/containers/json" else 404)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            self.close_connection = True

        def log_message(self, *args):
            pass

    with Server(str(socket), Handler) as server:
        os.chmod(socket, 0o600)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        started = time.monotonic()
        try:
            result = subprocess.run([binary, "read-snapshot", str(profile)],
                capture_output=True, text=True, timeout=8, check=True)
            elapsed = (time.monotonic() - started) * 1000
            assert result.stdout.strip() == str(count), result
            assert calls == ["/containers/json"], calls
            print(json.dumps({"containers": count, "requests": len(calls),
                "elapsedMs": round(elapsed, 2), "payloadBytes": len(payload)}))
        finally:
            server.shutdown()
            worker.join(timeout=2)
    socket.unlink()
    profile.rmdir()


for size in (1, 10, 100):
    measure(sys.argv[1], Path(sys.argv[2]), size)
