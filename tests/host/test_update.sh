#!/bin/bash
# Immutable-release updates must recover binary and guest selection changes
# when the installer fails or the updater is interrupted.
set -euo pipefail

HAMN=${HAMN:-build/hamn}
INSTALL=scripts/install-host.sh
WORK=$(mktemp -d /tmp/hamn-update.XXXXXX)
cleanup() {
    rm -rf "$WORK"
    make host VERSION=0.0.1-dev >/dev/null
}
trap cleanup EXIT

sha256() {
    shasum -a 256 "$1" | awk '{print $1}'
}

selection_hash() {
    sha256 "$HOME_DIR/.hamn/cache/guest-image.json"
}

assert_active_state() {
    local expected_target=$1 expected_selection=$2 label=$3
    [ "$(readlink "$BINDIR/hamn")" = "$expected_target" ] || {
        echo "FAIL: $label changed the managed binary target" >&2
        exit 1
    }
    [ "$(selection_hash)" = "$expected_selection" ] || {
        echo "FAIL: $label changed the guest image selection" >&2
        exit 1
    }
    [ ! -e "$HOME_DIR/.hamn/cache/.hamn-update-transaction" ] &&
        [ ! -L "$HOME_DIR/.hamn/cache/.hamn-update-transaction" ] || {
        echo "FAIL: $label left an active update transaction" >&2
        exit 1
    }
}

run_update() {
    HOME="$HOME_DIR" \
    HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS=1 \
        "$BINDIR/hamn" update --manifest "$1"
}

build_release() {
    local version=$1 guest_text=$2 install_mode=$3
    local root=$WORK/hamn-v$version-darwin-arm64
    local archive=$WORK/host-v$version.tar.gz
    local guest=$WORK/guest-v$version.img
    local manifest=$WORK/manifest-v$version.json
    local host_hash guest_hash

    make host VERSION="$version" >/dev/null
    mkdir -p "$root/bin"
    COPYFILE_DISABLE=1 cp build/hamn "$root/bin/hamn"
    rsync -a --exclude '._*' scripts packaging "$root/"
    mkdir -p "$root/packaging/release"
    printf '%s\n' 'https://example.invalid/hamn-update-manifest.json' \
        >"$root/packaging/release/update-manifest-url"
    chmod 0644 "$root/packaging/release/update-manifest-url"
    if [ "$install_mode" = fail ]; then
        printf '%s\n' '#!/bin/bash' 'exit 77' >"$root/scripts/install-host.sh"
        chmod 0755 "$root/scripts/install-host.sh"
    fi
    COPYFILE_DISABLE=1 tar -C "$WORK" -czf "$archive" "$(basename "$root")"
    printf '%s\n' "$guest_text" >"$guest"
    host_hash=$(sha256 "$archive")
    guest_hash=$(sha256 "$guest")
    printf '%s' \
        '{"schemaVersion":2,"channel":"stable","version":"v'"$version"'",' \
        '"commit":"0123456789abcdef0123456789abcdef01234567",' \
        '"validationMode":"github-hosted-no-vm",' \
        '"compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},' \
        '"artifacts":{"host":{"url":"file://'"$archive"'","sha256":"'"$host_hash"'"},' \
        '"guestImage":{"url":"file://'"$guest"'","sha256":"'"$guest_hash"'"}}}' \
        >"$manifest"
    printf '%s\n' "$manifest"
}

await_ready() {
    local fifo=$1 pid=$2 marker
    while ! IFS= read -r marker <"$fifo"; do
        kill -0 "$pid" 2>/dev/null || {
            echo "FAIL: update exited before its barrier became ready" >&2
            exit 1
        }
    done
    [ "$marker" = ready ] || {
        echo "FAIL: update barrier did not become ready" >&2
        exit 1
    }
}

HOME_DIR=$WORK/home
BINDIR=$WORK/bin
DATADIR=$WORK/share/hamn/src
mkdir -p "$HOME_DIR"
cp "$HAMN" "$WORK/old-hamn"
chmod 0755 "$WORK/old-hamn"
bash "$INSTALL" "$WORK/old-hamn" "$BINDIR" "$DATADIR" \
    >"$WORK/install.out"
old_target=$(readlink "$BINDIR/hamn")
MANAGED_BINDIR=$(cd "$BINDIR" && pwd -P)
MANAGED_DATADIR=$(cd "$DATADIR" && pwd -P)

MANIFEST_2=$(build_release 0.0.2 'immutable guest image v0.0.2' normal)
run_update "$MANIFEST_2" >"$WORK/update.out"
grep -Fq 'selected guest image is used for new profile disks' "$WORK/update.out"
new_target=$(readlink "$BINDIR/hamn")
[ "$new_target" != "$old_target" ] || {
    echo "FAIL: signed update did not switch the managed binary" >&2
    exit 1
}
HOME="$HOME_DIR" "$BINDIR/hamn" version | grep -Fxq 'hamn 0.0.2'
grep -Fq 'hamn-guest-' "$HOME_DIR/.hamn/cache/guest-image.json"
selection_2=$(selection_hash)

# A direct generation binary cannot update itself, and a modified manifest
# cannot change either selected generation or guest image.
if HOME="$HOME_DIR" "$new_target" update --manifest "$MANIFEST_2" \
    >"$WORK/direct.out" 2>"$WORK/direct.err"; then
    echo "FAIL: direct generation binary was accepted for update" >&2
    exit 1
fi
grep -Fq 'managed hamn command symlink' "$WORK/direct.err"
cp "$MANIFEST_2" "$WORK/bad-manifest.json"
printf '{' >"$WORK/bad-manifest.json"
if run_update "$WORK/bad-manifest.json" \
    >"$WORK/bad.out" 2>"$WORK/bad.err"; then
    echo "FAIL: modified manifest was accepted" >&2
    exit 1
fi
assert_active_state "$new_target" "$selection_2" 'manifest rejection'

# An installer failure occurs after both payloads are staged but before either
# public pointer may change.
MANIFEST_FAIL=$(build_release 0.0.3 'immutable guest image v0.0.3 failed' fail)
if run_update "$MANIFEST_FAIL" >"$WORK/fail.out" 2>"$WORK/fail.err"; then
    echo "FAIL: failing host installer was accepted" >&2
    exit 1
fi
assert_active_state "$new_target" "$selection_2" 'host installer failure'

# These tests run the managed helper itself so SIGKILL reaches the updater,
# rather than the command supervisor which deliberately forwards TERM.
HELPER=$(dirname "$(dirname "$new_target")")/share/hamn/src/scripts/update-host.sh
[ -x "$HELPER" ] || {
    echo "FAIL: cannot locate managed update helper" >&2
    exit 1
}
MANIFEST_3=$(build_release 0.0.3 'immutable guest image v0.0.3' normal)

TERM_READY=$WORK/term-ready
TERM_RELEASE=$WORK/term-release
mkfifo "$TERM_READY" "$TERM_RELEASE"
HOME="$HOME_DIR" \
HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS=1 \
HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_READY_FIFO="$TERM_READY" \
HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_RELEASE_FIFO="$TERM_RELEASE" \
    /bin/bash "$HELPER" --bindir "$MANAGED_BINDIR" --datadir "$MANAGED_DATADIR" \
    --manifest "$MANIFEST_3" >"$WORK/term.out" 2>"$WORK/term.err" &
term_pid=$!
await_ready "$TERM_READY" "$term_pid"
kill -TERM "$term_pid"
if wait "$term_pid"; then
    echo "FAIL: TERM did not interrupt the update" >&2
    exit 1
fi
assert_active_state "$new_target" "$selection_2" 'TERM interruption'

KILL_READY=$WORK/kill-ready
KILL_RELEASE=$WORK/kill-release
mkfifo "$KILL_READY" "$KILL_RELEASE"
HOME="$HOME_DIR" \
HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS=1 \
HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_READY_FIFO="$KILL_READY" \
HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_RELEASE_FIFO="$KILL_RELEASE" \
    /bin/bash "$HELPER" --bindir "$MANAGED_BINDIR" --datadir "$MANAGED_DATADIR" \
    --manifest "$MANIFEST_3" >"$WORK/kill.out" 2>"$WORK/kill.err" &
kill_pid=$!
await_ready "$KILL_READY" "$kill_pid"
kill -KILL "$kill_pid"
if wait "$kill_pid"; then
    echo "FAIL: SIGKILL did not terminate the update" >&2
    exit 1
fi
[ -e "$HOME_DIR/.hamn/cache/.hamn-update-transaction" ] || {
    echo "FAIL: SIGKILL did not leave a durable recovery transaction" >&2
    exit 1
}
[ "$(readlink "$BINDIR/hamn")" != "$new_target" ] || {
    echo "FAIL: SIGKILL did not reach the host cutover boundary" >&2
    exit 1
}
if HOME="$HOME_DIR" "$BINDIR/hamn" start --template=false \
    >"$WORK/pending-start.out" 2>"$WORK/pending-start.err"; then
    echo "FAIL: pending update transaction allowed VM start" >&2
    exit 1
fi
grep -Fq 'interrupted update recovery is pending' "$WORK/pending-start.err"

# The next update recovers before it validates the new manifest. A tampered
# manifest therefore proves recovery without allowing another cutover.
if run_update "$WORK/bad-manifest.json" \
    >"$WORK/recover.out" 2>"$WORK/recover.err"; then
    echo "FAIL: recovered update accepted a modified manifest" >&2
    exit 1
fi
grep -Fq 'recovered the previous binary and guest image selection' \
    "$WORK/recover.err"
assert_active_state "$new_target" "$selection_2" 'SIGKILL recovery'

run_update "$MANIFEST_3" >"$WORK/update-3.out"
target_3=$(readlink "$BINDIR/hamn")
[ "$target_3" != "$new_target" ] || {
    echo "FAIL: recovered updater could not perform a later signed update" >&2
    exit 1
}
HOME="$HOME_DIR" "$BINDIR/hamn" version | grep -Fxq 'hamn 0.0.3'
[ "$(selection_hash)" != "$selection_2" ] || {
    echo "FAIL: later signed update did not change guest image selection" >&2
    exit 1
}
[ ! -e "$HOME_DIR/.hamn/cache/.hamn-update-transaction" ] &&
    [ ! -L "$HOME_DIR/.hamn/cache/.hamn-update-transaction" ]

echo "PASS: immutable update rolls back installer failure and interruption safely"
