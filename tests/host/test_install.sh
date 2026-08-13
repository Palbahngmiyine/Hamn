#!/bin/bash
set -euo pipefail

source tests/host/fixtures/bounded_wait.sh

HAMN=${HAMN:-build/hamn}
INSTALL=scripts/install-host.sh
WORK=$(mktemp -d)
install_one=
install_two=
install_child_one=
install_child_two=
install_release_one=
install_release_two=
install_fifo_open=0
cleanup() {
    for child in "$install_child_one" "$install_child_two"; do
        if [ -n "$child" ] && kill -0 "$child" 2>/dev/null; then
            kill -CONT "$child" 2>/dev/null || true
            kill -KILL "$child" 2>/dev/null || true
        fi
    done
    for pid in "$install_one" "$install_two"; do
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            kill -KILL "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    if [ "$install_fifo_open" -eq 1 ]; then
        exec 7>&-
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

file_hash() {
    local output
    output=$(shasum -a 256 "$1")
    printf '%s\n' "${output%% *}"
}

# GitHub macOS runners do not provide /usr/bin/lockf. Perl is already required
# for descriptor identity checks, so keep installation locking on its flock.
if grep -q '/usr/bin/lockf' "$INSTALL"; then
    echo "FAIL: installer requires unavailable /usr/bin/lockf" >&2
    exit 1
fi

assert_managed_install() {
    local bindir=$1
    local datadir=$2
    local source=$3
    local canonical_parent
    local canonical_data
    local target
    local generation
    local expected_hash
    [ -L "$bindir/hamn" ]
    [ ! -e "$bindir/docker" ] && [ ! -L "$bindir/docker" ] || {
        echo "FAIL: Hamn installed or replaced a Docker CLI command" >&2
        return 1
    }
    canonical_parent=$(cd "$(dirname "$datadir")" && pwd -P)
    canonical_data=$canonical_parent/$(basename "$datadir")
    target=$(readlink "$bindir/hamn")
    case "$target" in
    "$canonical_data/.hamn-generations/"*"/bin/hamn") ;;
    *)
        echo "FAIL: hamn does not target its canonical generation" >&2
        return 1
        ;;
    esac
    generation=${target%/bin/hamn}
    [ -d "$generation" ] && [ ! -L "$generation" ]
    [ -f "$generation/.hamn-generation" ]
    [ "$(stat -f '%Lp:%l' "$generation/.hamn-generation")" = 600:1 ]
    expected_hash=$(file_hash "$source")
    grep -q "^binary_sha256=$expected_hash$" \
        "$generation/.hamn-generation"
    [ "$(file_hash "$target")" = "$expected_hash" ]
    cmp -s "$source" "$target"
    [ -x "$generation/share/hamn/src/scripts/update-host.sh" ]
    [ -d "$generation/share/hamn/src/packaging/release" ]
    [ ! -e "$generation/share/hamn/src/guest" ]
    [ ! -e "$generation/share/hamn/src/shared" ]
    [ ! -e "$generation/share/hamn/src/vendor" ]
    [ -f "$datadir/.hamn-managed" ]
    [ "$(<"$datadir/.hamn-managed")" = version=1 ]
}

# Fixed lock paths are untrusted. A symlink must be rejected before chmod/open.
LOCK_BIN="$WORK/lock-bin"
LOCK_DATA="$WORK/lock-share/hamn/src"
LOCK_TARGET="$WORK/lock-target"
mkdir -p "$LOCK_BIN" "$(dirname "$LOCK_DATA")"
printf '%s\n' keep-lock >"$LOCK_TARGET"
chmod 0644 "$LOCK_TARGET"
ln -s "$LOCK_TARGET" "$LOCK_BIN/.hamn-install.lock"
if bash "$INSTALL" "$HAMN" "$LOCK_BIN" "$LOCK_DATA" \
    >"$WORK/lock.out" 2>"$WORK/lock.err"; then
    echo "FAIL: installer followed a symlinked lock" >&2
    exit 1
fi
grep -q 'refusing unsafe install lock path' "$WORK/lock.err"
grep -q '^keep-lock$' "$LOCK_TARGET"
[ "$(stat -f '%Lp' "$LOCK_TARGET")" = 644 ]

# A fresh install with spaces publishes an immutable generation without
# creating a Docker shim. Docker CLI ownership remains external to Hamn.
BINDIR="$WORK/bin with space"
DATADIR="$WORK/share with space/hamn/src"
bash "$INSTALL" "$HAMN" "$BINDIR" "$DATADIR" >"$WORK/install.out"
assert_managed_install "$BINDIR" "$DATADIR" "$HAMN"
HOME="$WORK/home" "$BINDIR/hamn" status >"$WORK/status.out"
grep -q '^profile: default$' "$WORK/status.out"

# Reinstall succeeds without mutating or deleting the previous generation.
old_target=$(readlink "$BINDIR/hamn")
bash "$INSTALL" "$HAMN" "$BINDIR" "$DATADIR" >"$WORK/reinstall.out"
assert_managed_install "$BINDIR" "$DATADIR" "$HAMN"
[ -f "$old_target" ]

# Legacy marker+hash ownership migrates automatically. Legacy runtime-source
# data is deliberately preserved; it is not an in-place install target.
LEGACY_BIN="$WORK/legacy-bin"
LEGACY_DATA="$WORK/legacy-share/hamn/src"
mkdir -p "$LEGACY_BIN" "$LEGACY_DATA/guest" "$LEGACY_DATA/vendor"
cp "$HAMN" "$LEGACY_BIN/hamn"
chmod 0755 "$LEGACY_BIN/hamn"
legacy_hash=$(file_hash "$LEGACY_BIN/hamn")
printf 'sha256 %s\n' "$legacy_hash" \
    >"$LEGACY_BIN/.hamn-binary.sha256"
chmod 0644 "$LEGACY_BIN/.hamn-binary.sha256"
touch "$LEGACY_DATA/.hamn-managed"
chmod 0644 "$LEGACY_DATA/.hamn-managed"
printf '%s\n' keep-legacy-guest >"$LEGACY_DATA/guest/sentinel"
printf '%s\n' keep-legacy-vendor >"$LEGACY_DATA/vendor/sentinel"
bash "$INSTALL" "$HAMN" "$LEGACY_BIN" "$LEGACY_DATA" \
    >"$WORK/legacy.out"
assert_managed_install "$LEGACY_BIN" "$LEGACY_DATA" "$HAMN"
[ "$(<"$LEGACY_DATA/.hamn-managed")" = version=1 ]
grep -q '^keep-legacy-guest$' "$LEGACY_DATA/guest/sentinel"
grep -q '^keep-legacy-vendor$' "$LEGACY_DATA/vendor/sentinel"

# An unmarked legacy executable is never run or adopted implicitly. Explicit
# adoption requires only the managed Hamn data marker and hashes content.
ADOPT_BIN="$WORK/adopt-bin"
ADOPT_DATA="$WORK/adopt-share/hamn/src"
mkdir -p "$ADOPT_BIN" "$ADOPT_DATA"
printf '%s\n' \
    '#!/bin/sh' \
    'touch "$SPOOF_SENTINEL"' \
    'exit 0' >"$ADOPT_BIN/hamn"
chmod 0755 "$ADOPT_BIN/hamn"
touch "$ADOPT_DATA/.hamn-managed"
chmod 0644 "$ADOPT_DATA/.hamn-managed"
if SPOOF_SENTINEL="$WORK/adopt-spoof-ran" \
    bash "$INSTALL" "$HAMN" "$ADOPT_BIN" "$ADOPT_DATA" \
    >"$WORK/adopt-refused.out" 2>"$WORK/adopt-refused.err"; then
    echo "FAIL: installer automatically adopted an unmarked executable" >&2
    exit 1
fi
grep -q 'HAMN_ADOPT_LEGACY=1' "$WORK/adopt-refused.err"
[ ! -e "$WORK/adopt-spoof-ran" ]
SPOOF_SENTINEL="$WORK/adopt-spoof-ran" HAMN_ADOPT_LEGACY=1 \
    bash "$INSTALL" "$HAMN" "$ADOPT_BIN" "$ADOPT_DATA" \
    >"$WORK/adopted.out"
[ ! -e "$WORK/adopt-spoof-ran" ]
assert_managed_install "$ADOPT_BIN" "$ADOPT_DATA" "$HAMN"

# Hardlinks and invalid legacy hashes are not ownership evidence.
HARD_BIN="$WORK/hard-bin"
HARD_DATA="$WORK/hard-share/hamn/src"
mkdir -p "$HARD_BIN" "$HARD_DATA"
cp "$HAMN" "$HARD_BIN/hamn"
chmod 0755 "$HARD_BIN/hamn"
ln "$HARD_BIN/hamn" "$WORK/hard-hamn-link"
touch "$HARD_DATA/.hamn-managed"
chmod 0644 "$HARD_DATA/.hamn-managed"
if HAMN_ADOPT_LEGACY=1 \
    bash "$INSTALL" "$HAMN" "$HARD_BIN" "$HARD_DATA" \
    >"$WORK/hard.out" 2>"$WORK/hard.err"; then
    echo "FAIL: installer adopted a hardlinked legacy binary" >&2
    exit 1
fi
cmp -s "$HAMN" "$HARD_BIN/hamn"

BAD_MARKER_BIN="$WORK/bad-marker-bin"
BAD_MARKER_DATA="$WORK/bad-marker-share/hamn/src"
mkdir -p "$BAD_MARKER_BIN" "$BAD_MARKER_DATA"
cp "$HAMN" "$BAD_MARKER_BIN/hamn"
chmod 0755 "$BAD_MARKER_BIN/hamn"
printf 'sha256 %064d\n' 0 >"$BAD_MARKER_BIN/.hamn-binary.sha256"
chmod 0644 "$BAD_MARKER_BIN/.hamn-binary.sha256"
touch "$BAD_MARKER_DATA/.hamn-managed"
chmod 0644 "$BAD_MARKER_DATA/.hamn-managed"
if bash "$INSTALL" "$HAMN" "$BAD_MARKER_BIN" "$BAD_MARKER_DATA" \
    >"$WORK/bad-marker.out" 2>"$WORK/bad-marker.err"; then
    echo "FAIL: installer trusted an invalid legacy binary marker" >&2
    exit 1
fi
grep -q 'invalid identity marker' "$WORK/bad-marker.err"

# Existing Docker commands are outside Hamn ownership and remain untouched.
FOREIGN_DOCKER_BIN="$WORK/foreign-docker-bin"
FOREIGN_DOCKER_DATA="$WORK/foreign-docker-share/hamn/src"
mkdir -p "$FOREIGN_DOCKER_BIN"
printf '%s\n' keep-docker >"$FOREIGN_DOCKER_BIN/docker"
chmod 0755 "$FOREIGN_DOCKER_BIN/docker"
bash "$INSTALL" "$HAMN" "$FOREIGN_DOCKER_BIN" "$FOREIGN_DOCKER_DATA" \
    >"$WORK/foreign-docker.out"
test -L "$FOREIGN_DOCKER_BIN/hamn"
grep -q '^keep-docker$' "$FOREIGN_DOCKER_BIN/docker"

UNMANAGED_DATA="$WORK/unmanaged-share/hamn/src"
mkdir -p "$UNMANAGED_DATA"
printf '%s\n' keep-data >"$UNMANAGED_DATA/foreign"
if bash "$INSTALL" "$HAMN" "$WORK/unmanaged-bin" "$UNMANAGED_DATA" \
    >"$WORK/unmanaged.out" 2>"$WORK/unmanaged.err"; then
    echo "FAIL: installer modified unmanaged data" >&2
    exit 1
fi
grep -q '^keep-data$' "$UNMANAGED_DATA/foreign"

FOREIGN_LINK_BIN="$WORK/foreign-link-bin"
FOREIGN_LINK_DATA="$WORK/foreign-link-share/hamn/src"
mkdir -p "$FOREIGN_LINK_BIN" "$FOREIGN_LINK_DATA"
printf '%s\n' foreign >"$WORK/foreign-hamn"
chmod 0755 "$WORK/foreign-hamn"
ln -s "$WORK/foreign-hamn" "$FOREIGN_LINK_BIN/hamn"
touch "$FOREIGN_LINK_DATA/.hamn-managed"
chmod 0644 "$FOREIGN_LINK_DATA/.hamn-managed"
if bash "$INSTALL" "$HAMN" "$FOREIGN_LINK_BIN" "$FOREIGN_LINK_DATA" \
    >"$WORK/foreign-link.out" 2>"$WORK/foreign-link.err"; then
    echo "FAIL: installer replaced a foreign hamn symlink" >&2
    exit 1
fi
[ "$(readlink "$FOREIGN_LINK_BIN/hamn")" = "$WORK/foreign-hamn" ]

# A managed symlink is trusted only while its generation marker and binary
# name/hash agree. Mutation fails closed and leaves the symlink untouched.
TAMPER_BIN="$WORK/tamper-bin"
TAMPER_DATA="$WORK/tamper-share/hamn/src"
bash "$INSTALL" "$HAMN" "$TAMPER_BIN" "$TAMPER_DATA" \
    >"$WORK/tamper-initial.out"
tamper_target=$(readlink "$TAMPER_BIN/hamn")
printf '%s\n' tampered >>"$tamper_target"
if bash "$INSTALL" "$HAMN" "$TAMPER_BIN" "$TAMPER_DATA" \
    >"$WORK/tamper.out" 2>"$WORK/tamper.err"; then
    echo "FAIL: installer trusted a mutated managed generation" >&2
    exit 1
fi
[ "$(readlink "$TAMPER_BIN/hamn")" = "$tamper_target" ]

# Managed roots must stay private from group/world writers; otherwise another
# local user could alter the executable or release support files behind a trusted link.
MODE_BIN="$WORK/mode-bin"
MODE_DATA="$WORK/mode-share/hamn/src"
bash "$INSTALL" "$HAMN" "$MODE_BIN" "$MODE_DATA" \
    >"$WORK/mode-initial.out"
mode_target=$(readlink "$MODE_BIN/hamn")
chmod 0775 "$MODE_DATA"
if bash "$INSTALL" "$HAMN" "$MODE_BIN" "$MODE_DATA" \
    >"$WORK/mode-data.out" 2>"$WORK/mode-data.err"; then
    echo "FAIL: installer trusted a group-writable data root" >&2
    exit 1
fi
[ "$(readlink "$MODE_BIN/hamn")" = "$mode_target" ]
chmod 0755 "$MODE_DATA"
chmod 0775 "$MODE_DATA/.hamn-generations"
if bash "$INSTALL" "$HAMN" "$MODE_BIN" "$MODE_DATA" \
    >"$WORK/mode-generations.out" 2>"$WORK/mode-generations.err"; then
    echo "FAIL: installer trusted a group-writable generation root" >&2
    exit 1
fi
[ "$(readlink "$MODE_BIN/hamn")" = "$mode_target" ]
chmod 0755 "$MODE_DATA/.hamn-generations"
chmod 0775 "$MODE_BIN"
if bash "$INSTALL" "$HAMN" "$MODE_BIN" "$MODE_DATA" \
    >"$WORK/mode-bindir.out" 2>"$WORK/mode-bindir.err"; then
    echo "FAIL: installer trusted a group-writable binary root" >&2
    exit 1
fi
[ "$(readlink "$MODE_BIN/hamn")" = "$mode_target" ]
chmod 0755 "$MODE_BIN"

# A kill before the sole Hamn commit leaves no executable link, but the data
# marker and complete immutable generation make retry safe.
FRESH_KILL_BIN="$WORK/fresh-kill-bin"
FRESH_KILL_DATA="$WORK/fresh-kill-share/hamn/src"
set +e
BASH_ENV="$PWD/tests/host/fixtures/kill_generation_publish.sh" \
    HAMN_KILL_GENERATION_PUBLISH=before \
    bash "$INSTALL" "$HAMN" "$FRESH_KILL_BIN" "$FRESH_KILL_DATA" \
    >"$WORK/fresh-kill.out" 2>"$WORK/fresh-kill.err"
fresh_kill_rc=$?
set -e
[ "$fresh_kill_rc" -ne 0 ]
[ ! -e "$FRESH_KILL_BIN/hamn" ] && [ ! -L "$FRESH_KILL_BIN/hamn" ]
[ -f "$FRESH_KILL_DATA/.hamn-managed" ]
fresh_generations=("$FRESH_KILL_DATA"/.hamn-generations/[0-9a-f]*-*)
[ "${#fresh_generations[@]}" -eq 1 ] &&
    [ -f "${fresh_generations[0]}/.hamn-generation" ] &&
    cmp -s "$HAMN" "${fresh_generations[0]}/bin/hamn" || {
    echo "FAIL: fresh pre-commit kill left no complete generation" >&2
    exit 1
}
bash "$INSTALL" "$HAMN" "$FRESH_KILL_BIN" "$FRESH_KILL_DATA" \
    >"$WORK/fresh-kill-retry.out"
assert_managed_install "$FRESH_KILL_BIN" "$FRESH_KILL_DATA" "$HAMN"
[ -f "${fresh_generations[0]}/bin/hamn" ]

# SIGKILL immediately before the symlink commit leaves the old generation
# active. SIGKILL immediately after it leaves a complete new generation.
cp "$HAMN" "$WORK/replacement-one"
printf '%s\n' replacement-one >>"$WORK/replacement-one"
chmod 0755 "$WORK/replacement-one"
before_target=$(readlink "$BINDIR/hamn")
set +e
BASH_ENV="$PWD/tests/host/fixtures/kill_generation_publish.sh" \
    HAMN_KILL_GENERATION_PUBLISH=before \
    bash "$INSTALL" "$WORK/replacement-one" "$BINDIR" "$DATADIR" \
    >"$WORK/before-killed.out" 2>"$WORK/before-killed.err"
before_rc=$?
set -e
[ "$before_rc" -ne 0 ]
[ "$(readlink "$BINDIR/hamn")" = "$before_target" ]
[ -f "$before_target" ]
assert_managed_install "$BINDIR" "$DATADIR" "$HAMN"
bash "$INSTALL" "$WORK/replacement-one" "$BINDIR" "$DATADIR" \
    >"$WORK/before-retry.out"
assert_managed_install "$BINDIR" "$DATADIR" "$WORK/replacement-one"
[ -f "$before_target" ]

cp "$HAMN" "$WORK/replacement-two"
printf '%s\n' replacement-two >>"$WORK/replacement-two"
chmod 0755 "$WORK/replacement-two"
set +e
BASH_ENV="$PWD/tests/host/fixtures/kill_generation_publish.sh" \
    HAMN_KILL_GENERATION_PUBLISH=after \
    bash "$INSTALL" "$WORK/replacement-two" "$BINDIR" "$DATADIR" \
    >"$WORK/after-killed.out" 2>"$WORK/after-killed.err"
after_rc=$?
set -e
[ "$after_rc" -ne 0 ]
assert_managed_install "$BINDIR" "$DATADIR" "$WORK/replacement-two"
bash "$INSTALL" "$WORK/replacement-two" "$BINDIR" "$DATADIR" \
    >"$WORK/after-retry.out"
assert_managed_install "$BINDIR" "$DATADIR" "$WORK/replacement-two"

# Concurrent installers serialize before the first source-copy operation.
CONCURRENT_BIN="$WORK/concurrent-bin"
CONCURRENT_DATA="$WORK/concurrent-share/hamn/src"
BLOCK_BIN="$WORK/block-rsync-bin"
INSTALL_EVENT="$WORK/install-event"
mkdir -p "$BLOCK_BIN"
mkfifo "$INSTALL_EVENT"
printf '%s\n' \
    '#!/bin/bash' \
    'if [ "${1:-}" = --server ]; then' \
    '    exec /usr/bin/rsync "$@"' \
    'fi' \
    'release="${INSTALL_EVENT}.release.$$"' \
    'mkfifo "$release"' \
    'printf "entered %s %s\n" "$$" "$release" >"$INSTALL_EVENT"' \
    'IFS= read -r _ <"$release"' \
    'rm -f "$release"' \
    'exec /usr/bin/rsync "$@"' >"$BLOCK_BIN/rsync"
chmod +x "$BLOCK_BIN/rsync"
export INSTALL_EVENT
exec 7<>"$INSTALL_EVENT"
install_fifo_open=1
PATH="$BLOCK_BIN:/usr/bin:/bin" \
    bash "$INSTALL" "$HAMN" "$CONCURRENT_BIN" "$CONCURRENT_DATA" \
    >"$WORK/concurrent-one.out" 2>"$WORK/concurrent-one.err" &
install_one=$!
bounded_fifo_read "$INSTALL_EVENT" "the first installer transaction entry" 5
IFS=' ' read -r install_event install_child_one install_release_one \
    <<<"$BOUNDED_WAIT_LINE"
[ "$install_event" = entered ]
PATH="$BLOCK_BIN:/usr/bin:/bin" \
    bash "$INSTALL" "$HAMN" "$CONCURRENT_BIN" "$CONCURRENT_DATA" \
    >"$WORK/concurrent-two.out" 2>"$WORK/concurrent-two.err" &
install_two=$!
if ! bounded_fifo_expect_no_line "$INSTALL_EVENT" \
    "a concurrent installer transaction entry" 1; then
    echo "FAIL: concurrent installers entered one transaction" >&2
    exit 1
fi
kill -0 "$install_two"
kill -0 "$install_child_one"
printf 'release\n' >"$install_release_one"
wait "$install_one"
install_one=
install_child_one=
install_release_one=
bounded_fifo_read "$INSTALL_EVENT" "the waiting installer transaction entry" 5
IFS=' ' read -r install_event install_child_two install_release_two \
    <<<"$BOUNDED_WAIT_LINE"
[ "$install_event" = entered ]
printf 'release\n' >"$install_release_two"
wait "$install_two"
install_two=
install_child_two=
install_release_two=
exec 7>&-
install_fifo_open=0
rm -f "$INSTALL_EVENT"
unset INSTALL_EVENT
assert_managed_install "$CONCURRENT_BIN" "$CONCURRENT_DATA" "$HAMN"

# Canonical overlap aliases are rejected before any generation is created.
OVERLAP="$WORK/overlap"
if bash "$INSTALL" "$HAMN" "$OVERLAP" "$OVERLAP" \
    >"$WORK/overlap.out" 2>"$WORK/overlap.err"; then
    echo "FAIL: installer accepted overlapping targets" >&2
    exit 1
fi
grep -q 'overlapping binary and data directories' "$WORK/overlap.err"

echo "OK: immutable host install ownership and atomic publish checks passed"
