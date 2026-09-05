#!/usr/bin/env python3
"""Exercise the real single-binary worker with an isolated HOME; never boot a VM."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

binary = Path(os.environ.get("HAMN", "target/debug/hamn")).resolve()
with tempfile.TemporaryDirectory(prefix="hamn-worker-") as directory:
    env = dict(os.environ, HOME=directory)

    def call(words, **arguments):
        request = dict(words=words.split(), timeout=30, tail=200, **arguments)
        result = subprocess.run(
            [binary, "__core-worker"], input=json.dumps(request),
            text=True, capture_output=True, env=env, timeout=10, check=True,
        )
        return json.loads(result.stdout)

    assert call("vm list") == {"Ok": []}
    assert not (Path(directory) / ".hamn").exists()
    assert "Err" in call("vm create", profile="test")
    assert not (Path(directory) / ".hamn").exists()
    created = call("vm create", profile="test", yes=True, cpu=2, memory=2)
    assert created["Ok"]["cpus"] == 2
    assert created["Ok"]["memoryMiB"] == 2048
    assert call("vm status", profile="test")["Ok"]["state"] == "stopped"
    assert "Err" in call("vm configure", profile="../escape", yes=True)
    assert "Err" in call("vm status", profile="missing")
    assert not (Path(directory) / ".hamn" / "missing").exists()
    assert len(call("vm list")["Ok"]) == 1
    result = subprocess.run([binary, "--headless", "vm", "status", "--profile", "test"],
                            capture_output=True, text=True, env=env, timeout=10, check=True)
    value = json.loads(result.stdout)
    assert value["ok"] is True and value["data"]["cpus"] == 2
    result = subprocess.run([binary, "--headless", "capabilities"],
                            capture_output=True, text=True, env=env, timeout=10, check=True)
    assert json.loads(result.stdout)["data"]["formats"] == ["json", "ndjson"]
    result = subprocess.run([binary, "--headless", "vm", "configure", "--profile", "test", "--yes", "--watch"],
                            capture_output=True, text=True, env=env, timeout=10)
    assert result.returncode != 0
    assert json.loads(result.stdout)["error"]["code"] == "invalidRequest"
    result = subprocess.run([binary], capture_output=True, text=True, env=env, timeout=10)
    assert result.returncode != 0 and "\x1b" not in result.stdout
print("core worker isolation and protocol: passed")
