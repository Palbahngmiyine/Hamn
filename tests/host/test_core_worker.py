#!/usr/bin/env python3
"""Exercise the real single-binary worker with an isolated HOME; never boot a VM."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tarfile

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
    assert created["Ok"]["migration"] == "current"
    config = Path(directory) / ".hamn/test/config.yaml"
    assert "kubernetes:" not in config.read_text()
    with config.open("a") as output:
        output.write('\nkubernetes:\n  enabled: false\n  version: "v1.30.0+k3s1"\n')
    assert call("vm status", profile="test")["Ok"]["migration"] == "pending"
    old_config = config.read_bytes()
    assert call("vm migrate", profile="test", yes=True)["Ok"]["migration"] == "pending"
    assert config.read_bytes() == old_config  # stopped profiles wait for next start
    assert "Ok" in call("vm configure", profile="test", yes=True, cpu=2)
    assert "kubernetes:" in config.read_text()  # edits cannot discard migration evidence
    assert call("vm status", profile="test")["Ok"]["state"] == "stopped"
    assert "Err" in call("vm configure", profile="../escape", yes=True)
    assert "Err" in call("vm status", profile="missing")
    assert not (Path(directory) / ".hamn" / "missing").exists()
    assert len(call("vm list")["Ok"]) == 1
    archive = Path(directory) / "diagnostics.tar"
    assert "Err" in call("vm diagnostics", profile="test", path=str(archive))
    assert not archive.exists()
    diagnostic = call("vm diagnostics", profile="test", path=str(archive), yes=True)
    assert diagnostic["Ok"]["redacted"] and diagnostic["Ok"]["bytes"] > 0, diagnostic
    with tarfile.open(archive) as bundle:
        assert len(bundle.getmembers()) >= 3
        assert not any("ed25519" in member.name for member in bundle.getmembers())
    assert "Err" in call("vm diagnostics", profile="test", path=str(archive), yes=True)
    assert "Err" in call("system update", yes=True)
    assert "Err" in call("system uninstall")
    # Legacy context records must not cause either external CLI to run at stop.
    fake_bin = Path(directory) / "bin"
    fake_bin.mkdir()
    invoked = Path(directory) / "external-cli-invoked"
    for tool in ("docker", "kubectl"):
        command = fake_bin / tool
        command.write_text(f"#!/bin/sh\n/usr/bin/touch '{invoked}'\nexit 99\n")
        command.chmod(0o700)
    env["PATH"] = str(fake_bin) + ":/usr/bin:/bin"
    state_file = Path(directory) / ".hamn/test/state.json"
    state_file.write_text(json.dumps({"state": "stopped", "prev_docker_context": "outside",
                                      "prev_kube_context": "outside"}))
    assert "Ok" in call("vm stop", profile="test", yes=True)
    assert not invoked.exists()
    assert "prev_docker_context" not in json.loads(state_file.read_text())
    assert "prev_kube_context" not in json.loads(state_file.read_text())
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
    result = subprocess.run([binary, "vm", "start", "--profile", "test"],
                            capture_output=True, text=True, env=env, timeout=10)
    assert json.loads(result.stdout)["error"]["code"] == "invalidRequest"
print("core worker isolation and protocol: passed")
