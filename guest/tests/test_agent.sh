#!/bin/bash
set -euo pipefail

AGENT_BIN=$1
WORK=$(mktemp -d)
SOCK="$WORK/agent.sock"
PID=
cleanup() {
    if [ -n "$PID" ]; then
        kill -KILL "$PID" 2>/dev/null || true
        wait "$PID" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

# The agent receives a mountInotify request only through the profile-local
# host socket.  Give its Linux test process a controlled virtiofs table so the
# endpoint can prove that it refreshes one existing regular file and rejects
# unsafe paths without depending on a real VM mount.
MOUNT_ROOT="$WORK/mount"
mkdir -p "$MOUNT_ROOT/nested"
printf 'before\n' >"$MOUNT_ROOT/nested/file.txt"
printf 'home %s virtiofs rw 0 0\n' "$MOUNT_ROOT" >"$WORK/mounts"
export HAMND_MOUNT_INOTIFY_MOUNTS_FILE="$WORK/mounts"

if "$AGENT_BIN" --unsupported >"$WORK/bad.out" 2>"$WORK/bad.err"; then
    echo "FAIL: agent accepted an unsupported argument" >&2
    exit 1
fi
grep -q 'usage: hamnd' "$WORK/bad.err"

"$AGENT_BIN" --sock "$SOCK" \
    >"$WORK/agent.out" 2>"$WORK/agent.err" &
PID=$!

agent_is_running() {
    local state
    state=$(/bin/ps -p "$PID" -o stat= 2>/dev/null) || return 1
    [[ "$state" != *Z* ]]
}

ready=0
deadline=$((SECONDS + 5))
while [ "$SECONDS" -lt "$deadline" ]; do
    if curl --fail --silent --max-time 0.2 --unix-socket "$SOCK" \
        http://localhost/v1/status >"$WORK/status.json"; then
        ready=1
        break
    fi
    if ! agent_is_running; then
        set +e
        wait "$PID"
        agent_rc=$?
        set -e
        PID=
        echo "FAIL: agent exited before readiness (status $agent_rc)" >&2
        cat "$WORK/agent.err" >&2
        exit 1
    fi
    /bin/sleep 0.05
done
if [ "$ready" -ne 1 ]; then
    echo "FAIL: agent did not become ready" >&2
    cat "$WORK/agent.err" >&2
    exit 1
fi

python3 - "$WORK/status.json" <<'PY'
import json
import sys
status = json.load(open(sys.argv[1], encoding="utf-8"))
assert status["agentVersion"] == "0.0.1"
assert status["protocolVersion"] == 3
assert status["dockerSocket"] == "/var/run/docker.sock"
assert status["criSocket"] == "unix:///run/containerd/containerd.sock"
assert status["kubernetesNamespace"] == "k8s.io"
assert isinstance(status["dockerReady"], bool)
assert isinstance(status["criReady"], bool)
PY
curl --fail --silent --head --max-time 2 --unix-socket "$SOCK" \
    http://localhost/_ping >/dev/null

curl --fail --silent --show-error --max-time 2 --unix-socket "$SOCK" \
    -X POST -H 'Content-Type: application/json' \
    --data '{"tag":"home","path":"nested/file.txt","mtimeSec":1700000000,"mtimeNsec":9}' \
    http://localhost/v1/mount-inotify >/dev/null
python3 - "$MOUNT_ROOT/nested/file.txt" <<'PY'
import os
import sys

stamp = os.stat(sys.argv[1]).st_mtime_ns
assert stamp == 1700000000000000009, stamp
PY

python3 - "$SOCK" <<'PY'
import socket
import sys

path = sys.argv[1]

def request(raw):
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(2)
    client.connect(path)
    client.sendall(raw)
    client.shutdown(socket.SHUT_WR)
    response = b""
    while True:
        chunk = client.recv(4096)
        if not chunk:
            break
        response += chunk
    client.close()
    return response.split(b"\r\n", 1)[0]

valid = b"GET /v1/status HTTP/1.1\r\nHost: local\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
assert b" 200 " in request(valid)

mount_invalid = [
    b'{"tag":"home","path":"../nested/file.txt","mtimeSec":1700000000,"mtimeNsec":9}',
    b'{"tag":"home","path":"nested/file.txt","mtimeSec":1.5,"mtimeNsec":9}',
    b'{"tag":"home","path":"nested/file.txt","mtimeSec":1700000000,"mtimeNsec":9,"extra":true}',
    b'{"tag":"home","tag":"home","path":"nested/file.txt","mtimeSec":1700000000,"mtimeNsec":9}',
]
for body in mount_invalid:
    raw = b"POST /v1/mount-inotify HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nContent-Length: " + str(len(body)).encode() + b"\r\nConnection: close\r\n\r\n" + body
    assert b" 400 " in request(raw), body

invalid_headers = [
    b"Content-Length: 1x\r\n",
    b"Content-Length:\r\n",
    b"Content-Length: 18446744073709551616\r\n",
    b"Content-Length: 0\r\nContent-Length: 0\r\n",
    b"Transfer-Encoding: chunked\r\n",
]
for header in invalid_headers:
    raw = b"GET /v1/status HTTP/1.1\r\nHost: local\r\n" + header + \
        b"Connection: close\r\n\r\n"
    assert b" 400 " in request(raw), header

long_query = b"GET /v1/status?x=" + b"a" * 1024 + \
    b" HTTP/1.1\r\nHost: local\r\nConnection: close\r\n\r\n"
assert b" 400 " in request(long_query)
PY

echo "OK: hamnd agent Docker/CRI status boundary passed"
