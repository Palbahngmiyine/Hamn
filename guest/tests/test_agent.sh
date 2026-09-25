#!/bin/bash
set -euo pipefail

AGENT_BIN=$1
# guest-test-fixture (guest/Makefile) sends raw requests and checks JSON.
FIXTURE=${2:?usage: test_agent.sh AGENT_BIN GUEST_TEST_FIXTURE}
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

"$FIXTURE" agent-status "$WORK/status.json"
curl --fail --silent --head --max-time 2 --unix-socket "$SOCK" \
    http://localhost/_ping >/dev/null

curl --fail --silent --show-error --max-time 2 --unix-socket "$SOCK" \
    -X POST -H 'Content-Type: application/json' \
    --data '{"tag":"home","path":"nested/file.txt","mtimeSec":1700000000,"mtimeNsec":9}' \
    http://localhost/v1/mount-inotify >/dev/null
[ "$("$FIXTURE" mtime-ns "$MOUNT_ROOT/nested/file.txt")" = 1700000000000000009 ]

# Raw requests: printf %b turns the literal \r\n in REQUEST into CRLF.
expect_status() {
    local status=$1 request=$2 line
    printf '%b' "$request" >"$WORK/request"
    line=$("$FIXTURE" unix-request "$SOCK" <"$WORK/request")
    case "$line" in
        *" $status "*) ;;
        *)
            echo "FAIL: expected HTTP $status, got '$line' for: $request" >&2
            exit 1
            ;;
    esac
}

expect_status 200 'GET /v1/status HTTP/1.1\r\nHost: local\r\nContent-Length: 0\r\nConnection: close\r\n\r\n'

for body in \
    '{"tag":"home","path":"../nested/file.txt","mtimeSec":1700000000,"mtimeNsec":9}' \
    '{"tag":"home","path":"nested/file.txt","mtimeSec":1.5,"mtimeNsec":9}' \
    '{"tag":"home","path":"nested/file.txt","mtimeSec":1700000000,"mtimeNsec":9,"extra":true}' \
    '{"tag":"home","tag":"home","path":"nested/file.txt","mtimeSec":1700000000,"mtimeNsec":9}'; do
    expect_status 400 "POST /v1/mount-inotify HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nContent-Length: ${#body}\r\nConnection: close\r\n\r\n$body"
done

for header in \
    'Content-Length: 1x\r\n' \
    'Content-Length:\r\n' \
    'Content-Length: 18446744073709551616\r\n' \
    'Content-Length: 0\r\nContent-Length: 0\r\n' \
    'Transfer-Encoding: chunked\r\n'; do
    expect_status 400 "GET /v1/status HTTP/1.1\r\nHost: local\r\n${header}Connection: close\r\n\r\n"
done

long_query=$(printf '%1024s' '' | tr ' ' a)
expect_status 400 "GET /v1/status?x=$long_query HTTP/1.1\r\nHost: local\r\nConnection: close\r\n\r\n"

echo "OK: hamnd agent Docker/CRI status boundary passed"
