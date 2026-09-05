#!/usr/bin/env python3
"""Verify explicit context routing and mutation preconditions without a cluster."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

binary = Path(os.environ.get("HAMN", "target/debug/hamn")).resolve()
requests = []


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def respond(self, data, status=200):
        body = json.dumps(data).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        requests.append(("GET", self.path, None))
        obj = {"apiVersion": "v1", "kind": "Pod", "metadata": {
            "name": "sample", "namespace": "default", "uid": "pod-original", "resourceVersion": "10"}}
        if "?" in self.path:
            self.respond({"apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": [obj]})
        else:
            self.respond(obj)

    def do_DELETE(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(("DELETE", self.path, body))
        self.respond({"apiVersion": "v1", "kind": "Status", "status": "Success", "code": 200})


with tempfile.TemporaryDirectory(prefix="hamn-kubernetes-") as directory:
    env = dict(os.environ, HOME=directory, PATH="/usr/bin:/bin")
    config_path = Path(directory) / "config"
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    config = {"apiVersion": "v1", "kind": "Config", "current-context": "production",
              "clusters": [{"name": "fixture", "cluster": {"server": f"http://127.0.0.1:{server.server_port}"}}],
              "contexts": [{"name": "dev", "context": {"cluster": "fixture", "namespace": "default"}}]}
    config_path.write_text(json.dumps(config))
    original = config_path.read_bytes()

    def run(*arguments):
        result = subprocess.run([binary, "--headless", *arguments, "--kubeconfig", config_path],
                                env=env, capture_output=True, text=True, timeout=15)
        return result.returncode, json.loads(result.stdout)

    try:
        rc, value = run("k8s", "contexts", "list")
        assert rc == 0 and value["data"][0]["name"] == "dev", value
        assert requests == []
        rc, value = run("k8s", "pods", "list", "--context", "dev")
        assert rc == 0 and value["data"][0]["metadata"]["name"] == "sample", value
        rc, value = run("k8s", "pods", "delete", "sample", "--context", "dev", "--namespace", "default", "--uid", "replaced", "--yes")
        assert rc != 0 and value["error"]["code"] == "conflict", value
        assert not any(method == "DELETE" for method, _, _ in requests)
        rc, value = run("k8s", "pods", "delete", "sample", "--context", "dev", "--namespace", "default", "--uid", "pod-original", "--yes")
        assert rc == 0, value
        assert requests[-1][2]["preconditions"] == {"uid": "pod-original", "resourceVersion": "10"}
        assert config_path.read_bytes() == original
        assert not (Path(directory) / ".hamn").exists()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
print("Kubernetes context routing and mutation identity: passed")
