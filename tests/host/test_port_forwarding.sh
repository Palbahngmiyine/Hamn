#!/bin/bash
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$REPO_ROOT/tests/host/fixtures/bounded_wait.sh"

WORK=$(mktemp -d)
TEST_BIN="$WORK/test-port-forwarding"
PROFILE="$WORK/profile"
STATE="$PROFILE/port-forwards.tsv"
EVENTS="$WORK/events.tsv"
UNRELATED=
STUBBORN=

cleanup() {
    unset PORT_TEST_BARRIER_NAME PORT_TEST_BARRIER_COUNTER \
        PORT_TEST_BARRIER_COUNT 2>/dev/null || true
    if [ -x "$TEST_BIN" ] && [ -d "$PROFILE" ]; then
        PORT_TEST_DIR="$PROFILE" PORT_TEST_EVENTS="$EVENTS" \
            "$TEST_BIN" cleanup >/dev/null 2>&1 || true
    fi
    if [ -n "$UNRELATED" ]; then
        kill "$UNRELATED" 2>/dev/null || true
        wait "$UNRELATED" 2>/dev/null || true
    fi
    if [ -n "$STUBBORN" ]; then
        kill -KILL "$STUBBORN" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

clang -DHAMN_TEST -std=c11 -Wall -Wextra \
    -Werror=implicit-function-declaration \
    -mmacosx-version-min=13.0 -Ihost -Ivendor \
    tests/host/test_port_forwarding.c \
    host/fwd/docker_observer.c host/fwd/ports.c host/fwd/udp_proxy.c \
    host/util/fs.c host/util/proc.c vendor/cjson/cJSON.c \
    -o "$TEST_BIN"

mkdir -p "$PROFILE/logs"
: >"$EVENTS"
export PORT_TEST_DIR="$PROFILE"
export PORT_TEST_EVENTS="$EVENTS"

"$TEST_BIN" inspect-fixtures
"$TEST_BIN" snapshot-fixture "$PROFILE"

UDP_EXECUTABLE=${HAMN_UDP_EXECUTABLE:-$TEST_BIN}
if [ -n "${HAMN_UDP_EXECUTABLE:-}" ]; then
    python3 "$REPO_ROOT/tests/host/test_udp_proxy.py" "$UDP_EXECUTABLE" \
        "$WORK/udp-proxy" --production
else
    python3 "$REPO_ROOT/tests/host/test_udp_proxy.py" "$UDP_EXECUTABLE" \
        "$WORK/udp-proxy"
fi

# Published host and container ports use the same parser. Verify both sides of
# the valid 1..65535 interval, including the values immediately outside it.
for specification in \
    127.0.0.1:1:80/tcp 127.0.0.1:65535:80/tcp \
    127.0.0.1:8080:1/tcp 127.0.0.1:8080:65535/tcp; do
    "$TEST_BIN" parse "$specification"
done
for specification in \
    127.0.0.1:0:80/tcp 127.0.0.1:65536:80/tcp \
    127.0.0.1:8080:0/tcp 127.0.0.1:8080:65536/tcp; do
    if "$TEST_BIN" parse "$specification" 2>"$WORK/parse.err"; then
        echo "FAIL: out-of-range port was accepted: $specification" >&2
        exit 1
    fi
    grep -q "invalid port number" "$WORK/parse.err"
done

state_count() {
    if [ -f "$STATE" ]; then
        wc -l <"$STATE" | tr -d ' '
    else
        printf '0\n'
    fi
}

assert_process_gone() {
    local pid=$1
    if kill -0 "$pid" 2>/dev/null; then
        echo "FAIL: UDP forward process $pid is still running" >&2
        exit 1
    fi
}

# TCP add returns to pending host-listener ownership after its exact control
# request. This helper process then exits, so reconcile can cancel and remove
# the stale host-only listener before any remote create was submitted.
"$TEST_BIN" add 127.0.0.1:48101:80/tcp
test "$(state_count)" -eq 1
awk -F '\t' \
    'NF == 11 && $1 == "tcp" && $3 == 48101 && $5 == 0 &&
     $8 == "pending" && $9 > 1 && $10 > 0' \
    "$STATE" \
    | grep -q .
if "$TEST_BIN" add 127.0.0.1:48101:80/tcp >/dev/null 2>&1; then
    echo "FAIL: pending recovery ownership did not reserve its listener" >&2
    exit 1
fi
"$TEST_BIN" reconcile ''
test ! -e "$STATE"
"$TEST_BIN" add 127.0.0.1:48101:80/tcp
"$TEST_BIN" commit 127.0.0.1:48101:80/tcp
"$TEST_BIN" commit 127.0.0.1:48101:80/tcp
awk -F '\t' \
    'NF == 11 && $3 == 48101 && $8 == "committed" &&
     $9 == 0 && $10 == 0 && $11 == 0' "$STATE" | grep -q .
"$TEST_BIN" remove 127.0.0.1:48101:80/tcp
test ! -e "$STATE"
"$TEST_BIN" remove 127.0.0.1:48101:80/tcp
if "$TEST_BIN" commit 127.0.0.1:48101:80/tcp >/dev/null 2>&1; then
    echo "FAIL: a missing forward was committed" >&2
    exit 1
fi
grep -q $'^add\t127.0.0.1\t48101$' "$EVENTS"
grep -q $'^cancel\t127.0.0.1\t48101$' "$EVENTS"

# The Docker observer synchronizes a complete inspect snapshot. New mappings
# become committed only after their host listener exists; a repeat is
# idempotent, removed mappings are stopped, and an ambiguous snapshot changes
# nothing.
"$TEST_BIN" sync 127.0.0.1:48230:80/tcp 127.0.0.1:48231:53/udp
SYNC_UDP_PID=$(awk -F '\t' '$1 == "udp" && $3 == 48231 { print $5 }' "$STATE")
test -n "$SYNC_UDP_PID"
kill -0 "$SYNC_UDP_PID"
awk -F '\t' '$3 == 48230 && $8 == "committed" { tcp = 1 }
              $3 == 48231 && $8 == "committed" { udp = 1 }
              END { exit !(tcp && udp) }' "$STATE"
test "$(grep -c $'^add\t127.0.0.1\t48230$' "$EVENTS")" -eq 1
"$TEST_BIN" sync 127.0.0.1:48230:80/tcp 127.0.0.1:48231:53/udp
test "$(grep -c $'^add\t127.0.0.1\t48230$' "$EVENTS")" -eq 1
"$TEST_BIN" sync 127.0.0.1:48232:81/tcp
test -z "$(awk -F '\t' '$3 == 48230 || $3 == 48231 { print }' "$STATE")"
assert_process_gone "$SYNC_UDP_PID"
if "$TEST_BIN" sync 127.0.0.1:48232:81/tcp 0.0.0.0:48232:82/tcp \
    >/dev/null 2>&1; then
    echo "FAIL: ambiguous Docker port snapshot was accepted" >&2
    exit 1
fi
awk -F '\t' '$3 == 48232 && $4 == 81 && $8 == "committed"' "$STATE" \
    | grep -q .
"$TEST_BIN" sync
test ! -e "$STATE"

# A late completion is scoped to its exact wrapper PID/start generation. Once
# an old reservation is removed and the same listener is claimed again, its
# stale commit and cleanup must leave the replacement record untouched.
"$TEST_BIN" add 127.0.0.1:48133:80/tcp
read -r OLD_PID OLD_SEC OLD_USEC <<<"$(
    awk -F '\t' '$3 == 48133 { print $9, $10, $11 }' "$STATE"
)"
"$TEST_BIN" remove 127.0.0.1:48133:80/tcp
"$TEST_BIN" add 127.0.0.1:48133:81/tcp
NEW_RECORD=$(awk -F '\t' '$3 == 48133 { print }' "$STATE")
"$TEST_BIN" commit-owned 127.0.0.1:48133:80/tcp \
    "$OLD_PID" "$OLD_SEC" "$OLD_USEC"
test "$(awk -F '\t' '$3 == 48133 { print }' "$STATE")" = "$NEW_RECORD"
"$TEST_BIN" remove-owned 127.0.0.1:48133:80/tcp \
    "$OLD_PID" "$OLD_SEC" "$OLD_USEC"
test "$(awk -F '\t' '$3 == 48133 { print }' "$STATE")" = "$NEW_RECORD"
"$TEST_BIN" remove 127.0.0.1:48133:81/tcp
test ! -e "$STATE"

# Serialized reconciliation may resolve an in-flight record only when that
# record itself is serialized. An unrelated operation must not promote a live
# foreground generation merely because its guest mapping is already visible.
/bin/sleep 30 &
UNRELATED=$!
read -r OWNER_SEC OWNER_USEC <<<"$(
    "$TEST_BIN" process-token "$UNRELATED"
)"
printf 'tcp\t127.0.0.1\t48134\t80\t0\t0\t0\tsubmitted\t%s\t%s\t%s\n' \
    "$UNRELATED" "$OWNER_SEC" "$OWNER_USEC" >"$STATE"
"$TEST_BIN" reconcile-serialized '127.0.0.1:48134->80/tcp'
awk -F '\t' '$3 == 48134 && $8 == "submitted" && $9 > 1' "$STATE" \
    | grep -q .
kill "$UNRELATED"
wait "$UNRELATED" 2>/dev/null || true
UNRELATED=
"$TEST_BIN" reconcile-serialized '127.0.0.1:48134->80/tcp'
awk -F '\t' '$3 == 48134 && $8 == "committed" && $9 == 0' "$STATE" \
    | grep -q .
"$TEST_BIN" cleanup
test ! -e "$STATE"

# Every macOS address maps to the same guest protocol/port, so wildcard and
# distinct loopback addresses conflict before Docker can become ambiguous.
"$TEST_BIN" add 0.0.0.0:48102:80/tcp
if "$TEST_BIN" add 127.0.0.1:48102:80/tcp >/dev/null 2>&1; then
    echo "FAIL: overlapping TCP listeners were accepted" >&2
    exit 1
fi
if "$TEST_BIN" add 127.0.0.2:48102:80/tcp >/dev/null 2>&1; then
    echo "FAIL: distinct macOS addresses mapped to one guest TCP port" >&2
    exit 1
fi
test "$(state_count)" -eq 1
"$TEST_BIN" cleanup
test ! -e "$STATE"

# Listener creation failures remove their reservation. A state-save failure is
# detected before listener creation, so even an injected cancel failure cannot
# leave a live untracked listener behind.
if FAIL_FORWARD_PORT=48103 "$TEST_BIN" add 127.0.0.1:48103:80/tcp \
    >/dev/null 2>&1; then
    echo "FAIL: injected TCP listener failure was accepted" >&2
    exit 1
fi
test ! -e "$STATE"
if HAMN_TEST_FS_FAIL_BEFORE_RENAME=1 FAIL_CANCEL_PORT=48104 \
    "$TEST_BIN" add 127.0.0.1:48104:80/tcp >/dev/null 2>&1; then
    echo "FAIL: injected TCP state-save failure was accepted" >&2
    exit 1
fi
test ! -e "$STATE"
if grep -q $'^add\t127.0.0.1\t48104$' "$EVENTS" ||
   grep -q $'^cancel\t127.0.0.1\t48104$' "$EVENTS"; then
    echo "FAIL: listener was touched before recovery state was reserved" >&2
    exit 1
fi

# A failed TCP cancel with a free SO_REUSEADDR bind is idempotent evidence that
# the listener is already absent, even while the SSH master remains alive.
"$TEST_BIN" add 127.0.0.1:48115:80/tcp
FAIL_CANCEL_PORT=48115 "$TEST_BIN" remove 127.0.0.1:48115:80/tcp
test ! -e "$STATE"

# A valid UDP relay persists its process start token and is stopped on remove.
"$TEST_BIN" add 127.0.0.1:48105:53/udp
UDP_PID=$(awk -F '\t' '$1 == "udp" { print $5 }' "$STATE")
awk -F '\t' \
    'NF == 11 && $1 == "udp" && $6 > 0 && $8 == "pending" &&
     $9 > 1 && $10 > 0' "$STATE" \
    | grep -q .
kill -0 "$UDP_PID"
"$TEST_BIN" remove 127.0.0.1:48105:53/udp
test ! -e "$STATE"
assert_process_gone "$UDP_PID"

# A missing state file must not authorize replacing the only identity evidence
# for a live relay. Legacy/malformed evidence is likewise preserved fail-closed;
# only a complete token for a process that is definitely gone may be replaced.
"$TEST_BIN" add 127.0.0.1:48119:53/udp
UDP_PID=$(awk -F '\t' '$3 == 48119 { print $5 }' "$STATE")
UDP_STATE_RECORD=$(cat "$STATE")
UDP_PID_RECORD=$(cat "$PROFILE/udp-127-0-0-1-48119.pid")
rm "$STATE"
if "$TEST_BIN" add 127.0.0.1:48119:53/udp >/dev/null 2>&1; then
    echo "FAIL: live UDP pidfile was replaced after state loss" >&2
    exit 1
fi
kill -0 "$UDP_PID"
test "$(cat "$PROFILE/udp-127-0-0-1-48119.pid")" = "$UDP_PID_RECORD"
test ! -e "$STATE"
printf '%s\n' "$UDP_STATE_RECORD" >"$STATE"
"$TEST_BIN" remove 127.0.0.1:48119:53/udp
assert_process_gone "$UDP_PID"

printf '4242\n' >"$PROFILE/udp-127-0-0-1-48121.pid"
if "$TEST_BIN" add 127.0.0.1:48121:53/udp >/dev/null 2>&1; then
    echo "FAIL: legacy UDP pidfile was replaced" >&2
    exit 1
fi
test "$(cat "$PROFILE/udp-127-0-0-1-48121.pid")" = 4242
test ! -e "$STATE"
rm "$PROFILE/udp-127-0-0-1-48121.pid"

printf '2147483647\t1\t1\n' \
    >"$PROFILE/udp-127-0-0-1-48121.pid"
"$TEST_BIN" add 127.0.0.1:48121:53/udp
UDP_PID=$(awk -F '\t' '$3 == 48121 { print $5 }' "$STATE")
test "$UDP_PID" -ne 2147483647
"$TEST_BIN" remove 127.0.0.1:48121:53/udp
assert_process_gone "$UDP_PID"

# A dead pending owner with no relay identity is removable only when a bind
# probe proves that no UDP listener exists. An occupied port remains tracked
# fail-closed until an exact process identity becomes available.
printf 'udp\t127.0.0.1\t48117\t53\t0\t0\t0\tpending\t999999\t1\t1\n' \
    >"$STATE"
"$TEST_BIN" reconcile ''
test ! -e "$STATE"
"$TEST_BIN" add 127.0.0.1:48118:53/udp
UDP_PID=$(awk -F '\t' '$3 == 48118 { print $5 }' "$STATE")
UDP_STATE_RECORD=$(cat "$STATE")
UDP_PID_RECORD=$(cat "$PROFILE/udp-127-0-0-1-48118.pid")
rm "$PROFILE/udp-127-0-0-1-48118.pid"
printf 'udp\t127.0.0.1\t48118\t53\t0\t0\t0\tpending\t999999\t1\t1\n' \
    >"$STATE"
"$TEST_BIN" reconcile ''
grep -q $'^udp\t127.0.0.1\t48118\t' "$STATE"
printf '%s\n' "$UDP_STATE_RECORD" >"$STATE"
printf '%s\n' "$UDP_PID_RECORD" \
    >"$PROFILE/udp-127-0-0-1-48118.pid"
"$TEST_BIN" remove 127.0.0.1:48118:53/udp
assert_process_gone "$UDP_PID"

# UDP listener rollback is observable by immediately rebinding the same port.
if HAMN_TEST_FS_FAIL_BEFORE_RENAME=1 \
    "$TEST_BIN" add 127.0.0.1:48106:53/udp >/dev/null 2>&1; then
    echo "FAIL: injected UDP state-save failure was accepted" >&2
    exit 1
fi
"$TEST_BIN" add 127.0.0.1:48106:53/udp
UDP_PID=$(awk -F '\t' '$1 == "udp" { print $5 }' "$STATE")
"$TEST_BIN" remove 127.0.0.1:48106:53/udp
assert_process_gone "$UDP_PID"

# Legacy TCP records remain cleanable. A mismatched UDP start token proves the
# recorded relay is gone, so stale tracking clears without signaling the PID.
printf 'tcp\t127.0.0.1\t48107\t80\t0\n' >"$STATE"
"$TEST_BIN" cleanup
grep -q $'^cancel\t127.0.0.1\t48107$' "$EVENTS"
/bin/sleep 30 &
UNRELATED=$!
printf 'udp\t127.0.0.1\t48108\t53\t%s\t1\t1\n' "$UNRELATED" >"$STATE"
printf '%s\n' "$UNRELATED" >"$PROFILE/udp-127-0-0-1-48108.pid"
"$TEST_BIN" add 127.0.0.1:48116:53/udp
VERIFIED_PID=$(awk -F '\t' '$3 == 48116 { print $5 }' "$STATE")
"$TEST_BIN" cleanup
kill -0 "$UNRELATED"
assert_process_gone "$VERIFIED_PID"
test ! -e "$STATE"
test ! -e "$PROFILE/udp-127-0-0-1-48108.pid"
test ! -e "$PROFILE/udp-127-0-0-1-48116.pid"

# A legacy PID-only record cannot distinguish the relay from PID reuse. Every
# destructive path fails and preserves both state and pidfile for inspection.
printf 'udp\t127.0.0.1\t48109\t53\t%s\n' "$UNRELATED" >"$STATE"
printf '%s\n' "$UNRELATED" >"$PROFILE/udp-127-0-0-1-48109.pid"
if "$TEST_BIN" cleanup 2>"$WORK/legacy.err"; then
    echo "FAIL: cleanup forgot a legacy UDP relay" >&2
    exit 1
fi
kill -0 "$UNRELATED"
grep -q 'refusing to stop unverified UDP forward process' "$WORK/legacy.err"
awk -F '\t' -v pid="$UNRELATED" \
    'NF == 11 && $1 == "udp" && $5 == pid && $6 == 0 && $7 == 0 &&
     $8 == "committed" && $9 == 0 && $10 == 0 && $11 == 0' "$STATE" \
    | grep -q .
test -e "$PROFILE/udp-127-0-0-1-48109.pid"
if "$TEST_BIN" remove 127.0.0.1:48109:53/udp \
    2>"$WORK/remove-legacy.err"; then
    echo "FAIL: remove forgot a legacy UDP relay" >&2
    exit 1
fi
kill -0 "$UNRELATED"
test -e "$STATE"
test -e "$PROFILE/udp-127-0-0-1-48109.pid"
if "$TEST_BIN" reconcile '' 2>"$WORK/reconcile-legacy.err"; then
    echo "FAIL: reconcile forgot a legacy UDP relay" >&2
    exit 1
fi
kill -0 "$UNRELATED"
test -e "$STATE"
test -e "$PROFILE/udp-127-0-0-1-48109.pid"
rm "$STATE" "$PROFILE/udp-127-0-0-1-48109.pid"
kill "$UNRELATED"
wait "$UNRELATED" 2>/dev/null || true
UNRELATED=

# A verified relay that ignores SIGTERM is escalated to SIGKILL only while its
# persisted identity still matches. Successful cleanup removes all tracking.
READY_FIFO="$WORK/stubborn-ready"
export PORT_TEST_READY_FIFO="$READY_FIFO"
mkfifo "$READY_FIFO"
STUBBORN=$("$TEST_BIN" spawn-ignore-sigterm)
bounded_fifo_read "$READY_FIFO" "the SIGTERM-resistant relay readiness"
read -r START_SEC START_USEC <<<"$("$TEST_BIN" process-token "$STUBBORN")"
printf 'udp\t127.0.0.1\t48110\t53\t%s\t%s\t%s\n' \
    "$STUBBORN" "$START_SEC" "$START_USEC" >"$STATE"
printf '%s\n' "$STUBBORN" >"$PROFILE/udp-127-0-0-1-48110.pid"
"$TEST_BIN" cleanup
test ! -e "$STATE"
test ! -e "$PROFILE/udp-127-0-0-1-48110.pid"
assert_process_gone "$STUBBORN"
STUBBORN=
unset PORT_TEST_READY_FIFO
rm "$READY_FIFO"

# Reconcile matches the exact translated guest endpoint. A same-port mapping
# on another loopback address cannot retain the macOS listener.
"$TEST_BIN" add 127.0.0.1:48111:80/tcp
"$TEST_BIN" commit 127.0.0.1:48111:80/tcp
"$TEST_BIN" reconcile '127.0.0.2:48111->80/tcp'
test ! -e "$STATE"

# A published guest UDP mapping is not host-ready without a matching relay
# PID/start token. Never promote a dead pending relay or retain a dead committed
# relay as healthy; preserve the evidence and return an observable error until
# absent inventory or VM-stop cleanup makes removal safe.
printf 'udp\t127.0.0.1\t48129\t53\t2147483647\t1\t1\tpending\t2147483646\t1\t1\n' \
    >"$STATE"
printf '2147483647\t1\t1\n' >"$PROFILE/udp-127-0-0-1-48129.pid"
if "$TEST_BIN" reconcile '192.0.2.10:48129->53/udp' \
    >/dev/null 2>&1; then
    echo "FAIL: dead pending UDP relay was promoted from guest inventory" >&2
    exit 1
fi
grep -q $'^udp\t127.0.0.1\t48129\t53\t2147483647\t1\t1\tpending\t' \
    "$STATE"
"$TEST_BIN" reconcile ''
test ! -e "$STATE"
test ! -e "$PROFILE/udp-127-0-0-1-48129.pid"

printf 'udp\t127.0.0.1\t48129\t53\t2147483647\t1\t1\tcommitted\t0\t0\t0\n' \
    >"$STATE"
printf '2147483647\t1\t1\n' >"$PROFILE/udp-127-0-0-1-48129.pid"
if "$TEST_BIN" reconcile '192.0.2.10:48129->53/udp' \
    >/dev/null 2>&1; then
    echo "FAIL: dead committed UDP relay remained host-ready" >&2
    exit 1
fi
grep -q $'^udp\t127.0.0.1\t48129\t53\t2147483647\t1\t1\tcommitted\t' \
    "$STATE"
"$TEST_BIN" reconcile ''
test ! -e "$STATE"
test ! -e "$PROFILE/udp-127-0-0-1-48129.pid"

# Reconcile retains committed published listeners, removes stale listeners,
# and handles an empty set. Exercise identity-checked UDP cleanup as well.
"$TEST_BIN" add 127.0.0.1:48111:80/tcp
"$TEST_BIN" add 127.0.0.1:48112:53/udp
"$TEST_BIN" commit 127.0.0.1:48111:80/tcp
"$TEST_BIN" commit 127.0.0.1:48112:53/udp
UDP_PID=$(awk -F '\t' '$1 == "udp" { print $5 }' "$STATE")
"$TEST_BIN" reconcile '127.0.0.1:48111->80/tcp'
test "$(state_count)" -eq 1
grep -q $'^tcp\t127.0.0.1\t48111\t' "$STATE"
assert_process_gone "$UDP_PID"
"$TEST_BIN" reconcile ''
test ! -e "$STATE"

# Docker groups mappings with one bind IP/protocol suffix and compresses
# consecutive ports into ranges. Preserve only exact host/container pairs.
for spec in \
    127.0.0.1:48120:80/tcp \
    127.0.0.1:48122:82/tcp \
    127.0.0.1:48123:83/tcp; do
    "$TEST_BIN" add "$spec"
    "$TEST_BIN" commit "$spec"
done
"$TEST_BIN" reconcile \
    '127.0.0.1:48120->80, 48122->82, 48123->84/tcp'
test "$(state_count)" -eq 2
grep -q $'^tcp\t127.0.0.1\t48120\t80\t' "$STATE"
grep -q $'^tcp\t127.0.0.1\t48122\t82\t' "$STATE"
if grep -q $'\t48123\t' "$STATE"; then
    echo "FAIL: grouped inventory ignored the container port pair" >&2
    exit 1
fi
"$TEST_BIN" cleanup

for spec in \
    127.0.0.1:48130:90/tcp \
    127.0.0.1:48131:91/tcp \
    127.0.0.1:48132:91/tcp; do
    "$TEST_BIN" add "$spec"
    "$TEST_BIN" commit "$spec"
done
"$TEST_BIN" reconcile '127.0.0.1:48130-48132->90-92/tcp'
test "$(state_count)" -eq 2
grep -q $'^tcp\t127.0.0.1\t48130\t90\t' "$STATE"
grep -q $'^tcp\t127.0.0.1\t48131\t91\t' "$STATE"
if grep -q $'\t48132\t' "$STATE"; then
    echo "FAIL: range inventory ignored the port offset" >&2
    exit 1
fi
"$TEST_BIN" cleanup

# Cleanup removes mixed TCP/UDP state and stops the UDP process.
"$TEST_BIN" add 127.0.0.1:48113:80/tcp
"$TEST_BIN" add 127.0.0.1:48114:53/udp
UDP_PID=$(awk -F '\t' '$1 == "udp" { print $5 }' "$STATE")
"$TEST_BIN" cleanup
test ! -e "$STATE"
assert_process_gone "$UDP_PID"

# Parallel process RMW must preserve every independently-added TCP record.
PORT_TEST_BARRIER_NAME="/hamn-port-add-$$"
PORT_TEST_BARRIER_COUNTER="$WORK/add-barrier-count"
PORT_TEST_BARRIER_COUNT=24
export PORT_TEST_BARRIER_NAME PORT_TEST_BARRIER_COUNTER \
    PORT_TEST_BARRIER_COUNT
PIDS=()
for port in {48200..48223}; do
    "$TEST_BIN" add "127.0.0.1:$port:80/tcp" &
    PIDS+=("$!")
done
for pid in "${PIDS[@]}"; do
    wait "$pid"
done
test "$(state_count)" -eq 24
test "$(awk -F '\t' '{ print $3 }' "$STATE" | sort -u | wc -l | tr -d ' ')" \
    -eq 24
unset PORT_TEST_BARRIER_NAME PORT_TEST_BARRIER_COUNTER \
    PORT_TEST_BARRIER_COUNT
rm -f "$WORK/add-barrier-count"

# Parallel remove is also an RMW operation and must not resurrect records.
PORT_TEST_BARRIER_NAME="/hamn-port-remove-$$"
PORT_TEST_BARRIER_COUNTER="$WORK/remove-barrier-count"
PORT_TEST_BARRIER_COUNT=24
export PORT_TEST_BARRIER_NAME PORT_TEST_BARRIER_COUNTER \
    PORT_TEST_BARRIER_COUNT
PIDS=()
for port in {48200..48223}; do
    "$TEST_BIN" remove "127.0.0.1:$port:80/tcp" &
    PIDS+=("$!")
done
for pid in "${PIDS[@]}"; do
    wait "$pid"
done
test ! -e "$STATE"
unset PORT_TEST_BARRIER_NAME PORT_TEST_BARRIER_COUNTER \
    PORT_TEST_BARRIER_COUNT
rm -f "$WORK/remove-barrier-count"

# Same-listener races have exactly one winner under the state lock.
"$TEST_BIN" add 127.0.0.1:48224:80/tcp & FIRST=$!
"$TEST_BIN" add 127.0.0.1:48224:80/tcp >/dev/null 2>&1 & SECOND=$!
SUCCESS=0
if wait "$FIRST"; then SUCCESS=$((SUCCESS + 1)); fi
if wait "$SECOND"; then SUCCESS=$((SUCCESS + 1)); fi
test "$SUCCESS" -eq 1
test "$(state_count)" -eq 1
"$TEST_BIN" cleanup

# UDP additions share the same process lock and preserve every PID/token pair.
PIDS=()
for port in {48225..48228}; do
    "$TEST_BIN" add "127.0.0.1:$port:53/udp" &
    PIDS+=("$!")
done
for pid in "${PIDS[@]}"; do
    wait "$pid"
done
test "$(state_count)" -eq 4
UDP_PIDS=$(awk -F '\t' '$1 == "udp" { print $5 }' "$STATE")
"$TEST_BIN" cleanup
for pid in $UDP_PIDS; do
    assert_process_gone "$pid"
done

# The fixed state capacity rejects both one more add and an oversized file.
: >"$STATE"
for offset in {0..127}; do
    printf 'tcp\t127.0.0.1\t%s\t80\t0\t0\t0\n' \
        "$((49000 + offset))" >>"$STATE"
done
if "$TEST_BIN" add 127.0.0.1:49200:80/tcp >/dev/null 2>&1; then
    echo "FAIL: port forward state exceeded its fixed capacity" >&2
    exit 1
fi
test "$(state_count)" -eq 128
printf 'tcp\t127.0.0.1\t49201\t80\t0\t0\t0\n' >>"$STATE"
if "$TEST_BIN" cleanup >/dev/null 2>&1; then
    echo "FAIL: oversized port forward state was partially accepted" >&2
    exit 1
fi
rm "$STATE"

# Corrupt state is rejected consistently rather than partially overwritten.
printf 'malformed\n' >"$STATE"
if "$TEST_BIN" add 127.0.0.1:48229:80/tcp >/dev/null 2>&1; then exit 1; fi
if "$TEST_BIN" commit 127.0.0.1:48229:80/tcp >/dev/null 2>&1; then exit 1; fi
if "$TEST_BIN" remove 127.0.0.1:48229:80/tcp >/dev/null 2>&1; then exit 1; fi
if "$TEST_BIN" reconcile '' >/dev/null 2>&1; then exit 1; fi
if "$TEST_BIN" cleanup >/dev/null 2>&1; then exit 1; fi
rm "$STATE"
for invalid_record in \
    $'tcp\tnot-an-ip\t49202\t80\t0\t0\t0' \
    $'udp\t127.0.0.1\t49202\t53\t42\t1\t1000000' \
    $'tcp\t127.0.0.1\t49202\t80\t0\t0' \
    $'tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tunknown' \
    $'tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tpending\textra'; do
    printf '%s\n' "$invalid_record" >"$STATE"
    if "$TEST_BIN" cleanup >/dev/null 2>&1; then
        echo "FAIL: malformed port forward record was accepted" >&2
        exit 1
    fi
done
rm "$STATE"

# Every public mutation fails before touching state when the process lock
# cannot be opened.
rm -f "$PROFILE/port-forwards.lock"
mkdir "$PROFILE/port-forwards.lock"
if "$TEST_BIN" add 127.0.0.1:49202:80/tcp >/dev/null 2>&1; then exit 1; fi
if "$TEST_BIN" commit 127.0.0.1:49202:80/tcp >/dev/null 2>&1; then exit 1; fi
if "$TEST_BIN" remove 127.0.0.1:49202:80/tcp >/dev/null 2>&1; then exit 1; fi
if "$TEST_BIN" reconcile '' >/dev/null 2>&1; then exit 1; fi
if "$TEST_BIN" cleanup >/dev/null 2>&1; then exit 1; fi
rmdir "$PROFILE/port-forwards.lock"

echo "PASS: process-safe TCP/UDP port forward state and cleanup"
