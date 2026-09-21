#!/usr/bin/env python3
"""Keep the headless Engine schema over an explicit real Docker CLI context."""
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import socketserver
import subprocess
import tempfile
import threading

binary = Path(os.environ.get("HAMN", "build/hamn")).resolve()
docker = shutil.which("docker")
if not docker:
    raise SystemExit("Docker CLI is required for external-context transport validation")
calls = []
mode = {"deny": False, "wait": False, "large": False}
release = threading.Event()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def reply(self):
        calls.append((self.command, self.path))
        path = re.sub(r"^/v[0-9.]+", "", self.path).split("?")[0]
        code, body = 200, {}
        if path == "/version":
            body = {"ApiVersion": "1.47", "MinAPIVersion": "1.24"}
        elif path == "/containers/json":
            if mode["wait"]:
                release.wait(timeout=5)
            body = [{"Id": "a" * 64, "Names": ["/external"], "State": "running"}]
            if mode["large"]:
                body = [{"Id": f"{i:064x}", "Names": [f"/row-{i}"], "Labels": {"padding": "x" * 4096}} for i in range(2048)]
        elif path.endswith("/json"):
            body = {"Id": "a" * 64, "Name": "/external", "State": {"Running": True}}
        elif path.endswith("/start"):
            code = 204
        if mode["deny"] and path != "/version":
            code, body = 403, {"message": "fixture permission denied"}
        data = b"" if code == 204 else json.dumps(body).encode()
        try:
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)
        except BrokenPipeError:
            pass
        self.close_connection = True

    do_GET = do_POST = reply

    def log_message(self, *args):
        pass


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


with tempfile.TemporaryDirectory(prefix="hamn-context-", dir="/tmp") as temp:
    root = Path(temp)
    home = root / "home"
    home.mkdir()
    socket = root / "engine.sock"
    env = dict(os.environ, HOME=str(home), DOCKER_CONFIG=str(root / "docker-config"),
        DOCKER_HOST="unix:///not-the-selected-engine.sock", DOCKER_CONTEXT="not-selected")
    env.pop("DOCKER_API_VERSION", None)
    subprocess.run([docker, "context", "create", "fixture", "--docker", f"host=unix://{socket}"],
        env=env, check=True, capture_output=True, timeout=10)

    def run(*arguments):
        result = subprocess.run([binary, "--headless", "docker", *arguments], env=env,
            text=True, capture_output=True, timeout=12)
        return result, json.loads(result.stdout)

    with Server(str(socket), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            result, data = run("containers", "list", "--context", "fixture")
            assert result.returncode == 0 and data["data"][0]["Id"] == "a" * 64, (result, data)
            assert data["target"]["context"] == "fixture" and data["target"]["profile"] is None
            config = env["DOCKER_CONFIG"]
            env["DOCKER_CONFIG"] = str(root / "wrong-config")
            result, explicit = run("containers", "list", "--context", "fixture", "--docker-config", config)
            assert result.returncode == 0 and explicit["target"]["dockerConfig"] == config, explicit
            env["DOCKER_CONFIG"] = config
            result, data = run("containers", "start", "external", "--context", "fixture", "--yes")
            assert result.returncode == 0, (result, data)
            assert any(method == "POST" and f"/containers/{'a' * 64}/start" in path for method, path in calls)
            count = len(calls)
            for args in [("containers", "list"),
                         ("containers", "list", "--profile", "default", "--context", "fixture"),
                         ("containers", "start", "external", "--context", "fixture")]:
                result, data = run(*args)
                assert result.returncode != 0 and data["error"]["code"] == "invalidRequest", data
                assert len(calls) == count
            result, data = run("containers", "list", "--context", "does-not-exist")
            assert result.returncode != 0 and len(calls) == count, data
            mode["large"] = True
            result, data = run("containers", "list", "--context", "fixture")
            assert result.returncode == 0 and len(data["data"]) == 2048 and data["data"][-1]["Names"] == ["/row-2047"], (result.returncode, data.get("error"))
            mode["large"] = False
            mode["deny"] = True
            result, data = run("containers", "list", "--context", "fixture")
            assert result.returncode != 0 and data["error"]["code"] == "permissionDenied", data
            mode["deny"], mode["wait"] = False, True
            result, data = run("containers", "list", "--context", "fixture", "--timeout", "1")
            assert result.returncode != 0 and data["error"]["code"] == "timeout", data
            # No VM status/migration/profile operation is permitted on this path.
            assert not (home / ".hamn").exists(), list(home.rglob("*"))
        finally:
            release.set()
            server.shutdown()
            thread.join(timeout=3)
print("PASS: explicit real Docker context, Engine schema, immutable mutation ID, no profile side effects, deadline")
