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
mode = {"uid": True, "delete": 200, "list": "normal"}


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
        if not mode["uid"]:
            del obj["metadata"]["uid"]
        if self.path.split('?')[0].endswith('/log'):
            body = "한글 Pod log\nlast line".encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        elif "?" in self.path:
            metadata = {"continue": "same"} if mode["list"] == "repeat" else {}
            self.respond({"apiVersion": "v1", "kind": "PodList", "metadata": metadata,
                          "items": [obj] if mode["list"] == "normal" else []})
        else:
            self.respond(obj)

    def do_DELETE(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(("DELETE", self.path, body))
        self.respond({"apiVersion": "v1", "kind": "Status", "status": "Success" if mode["delete"] == 200 else "Failure",
                      "message": "fixture", "reason": "Fixture", "code": mode["delete"]}, mode["delete"])


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
        missing = subprocess.run([binary, '--headless', 'k8s', 'contexts', 'list'],
            env=dict(env, KUBECONFIG=''), capture_output=True, text=True, timeout=15)
        assert missing.returncode == 0 and json.loads(missing.stdout)['data'] == [], missing
        rc, value = run("k8s", "contexts", "list")
        assert rc == 0 and value["data"][0]["name"] == "dev", value
        assert requests == []
        absent = Path(directory) / 'absent'
        malformed = Path(directory) / 'malformed'
        malformed.write_text('contexts: [invalid yaml')
        for paths in [str(absent) + os.pathsep + str(config_path),
                      str(config_path) + os.pathsep + str(absent), str(absent)]:
            merged = subprocess.run([binary, '--headless', 'k8s', 'contexts', 'list'],
                env=dict(env, KUBECONFIG=paths), capture_output=True, text=True, timeout=15)
            expected = [] if paths == str(absent) else ['dev']
            assert merged.returncode == 0, merged
            assert [row['name'] for row in json.loads(merged.stdout)['data']] == expected
        # Missing environment entries are optional; explicit or malformed input is not.
        for paths, flags in [(str(config_path), ['--kubeconfig', str(absent)]),
                             (str(malformed) + os.pathsep + str(config_path), [])]:
            rejected = subprocess.run([binary, '--headless', 'k8s', 'contexts', 'list', *flags],
                env=dict(env, KUBECONFIG=paths), capture_output=True, text=True, timeout=15)
            assert rejected.returncode != 0, rejected
            assert json.loads(rejected.stdout)['error']['code'] == 'configurationInvalid'
        assert requests == []
        rc, value = run("k8s", "pods", "list", "--context", "dev")
        assert rc == 0 and value["data"][0]["metadata"]["name"] == "sample", value
        rc, value = run("k8s", "pods", "delete", "sample", "--context", "dev", "--namespace", "default", "--uid", "replaced", "--yes")
        assert rc != 0 and value["error"]["code"] == "conflict", value
        assert not any(method == "DELETE" for method, _, _ in requests)
        rc, value = run("k8s", "pods", "delete", "sample", "--context", "dev", "--namespace", "default", "--uid", "pod-original", "--yes")
        assert rc == 0, value
        assert requests[-1][2]["preconditions"] == {"uid": "pod-original", "resourceVersion": "10"}
        for flags in ([], ['--follow']):
            logs = subprocess.run([binary, '--headless', 'k8s', 'pods', 'logs', 'sample',
                '--context', 'dev', '--namespace', 'default', '--kubeconfig', config_path, *flags],
                env=env, capture_output=True, text=True, timeout=15)
            records = [json.loads(line) for line in logs.stdout.splitlines()]
            assert logs.returncode == 0 and len(records) == 3, (records, logs.stderr)
            assert records[0]['data']['text'] == '한글 Pod log\n'
            assert records[1]['data']['text'] == 'last line'
            assert records[-1]['type'] == 'result' and records[-1]['ok']
        mode["delete"] = 503
        before = len(requests)
        rc, value = run("k8s", "pods", "delete", "sample", "--context", "dev", "--namespace", "default", "--yes")
        assert rc != 0 and value["error"]["code"] == "outcomeUnknown", value
        assert len(requests) == before + 2  # one identity GET and exactly one DELETE
        mode["delete"] = 403
        rc, value = run("k8s", "pods", "delete", "sample", "--context", "dev", "--namespace", "default", "--yes")
        assert rc != 0 and value["error"]["code"] == "permissionDenied", value
        mode["uid"] = False
        before = len(requests)
        rc, value = run("k8s", "pods", "delete", "sample", "--context", "dev", "--namespace", "default", "--yes")
        assert rc != 0 and value["error"]["code"] == "invalidResponse", value
        assert all(method != "DELETE" for method, _, _ in requests[before:])
        mode["list"] = "empty"
        assert run("k8s", "pods", "list", "--context", "dev")[1]["data"] == []
        mode["list"] = "repeat"
        before = len(requests)
        rc, value = run("k8s", "pods", "list", "--context", "dev")
        assert rc != 0 and value["error"]["code"] == "invalidResponse", value
        assert len(requests) == before + 2
        assert config_path.read_bytes() == original
        assert not (Path(directory) / ".hamn").exists()
        legacy = {"apiVersion": "v1", "kind": "Config", "current-context": "hamn",
                  "contexts": [{"name": "hamn", "context": {"cluster": "hamn", "user": "hamn"}}],
                  "clusters": [{"name": "hamn", "cluster": {"server": "https://127.0.0.1:16443"}}]}
        config_path.write_text(json.dumps(legacy))
        legacy_bytes = config_path.read_bytes()
        marker_dir = Path(directory) / '.hamn/.kube-contexts'
        marker_dir.mkdir(parents=True)
        marker = marker_dir / 'default'
        marker.write_text('schema=1\ncontext=hamn\n')
        for retired in [False, True]:
            if retired:
                marker_dir.rename(marker_dir.with_name('.retired-kube-contexts'))
            assert run('k8s', 'contexts', 'list')[1]['data'][0]['available'] is False
            rc, value = run('k8s', 'pods', 'list', '--context', 'hamn')
            assert rc != 0 and value['error']['code'] == 'managedK3sRemoved', value
            assert config_path.read_bytes() == legacy_bytes
        # Reusing the context name for a real external endpoint remains allowed.
        legacy['clusters'][0]['cluster']['server'] = 'https://external.example:6443'
        config_path.write_text(json.dumps(legacy))
        assert run('k8s', 'contexts', 'list')[1]['data'][0]['available'] is True
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
print("Kubernetes context routing and mutation identity: passed")
