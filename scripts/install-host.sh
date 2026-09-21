#!/bin/bash
set -euo pipefail
export LC_ALL=C

if [ "$#" -ne 3 ] && { [ "$#" -ne 4 ] || [ -z "${4:-}" ]; }; then
    echo "usage: install-host.sh HAMN_BINARY BINDIR DATADIR [UPDATE_JOURNAL]" >&2
    exit 2
fi

SOURCE=$1
BINDIR=$2
DATADIR=$3
UPDATE_JOURNAL=${4:-}
DATA_PARENT=$(dirname "$DATADIR")
DATA_BASE=$(basename "$DATADIR")
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

source "$ROOT/scripts/install-support.sh"

case "$DATA_BASE" in
''|.|..|/)
    echo "hamn: refusing non-canonical data directory: $DATADIR" >&2
    exit 1
    ;;
esac
[ -f "$SOURCE" ] && [ ! -L "$SOURCE" ] && [ -x "$SOURCE" ] || {
    echo "hamn: install source is not a regular executable: $SOURCE" >&2
    exit 1
}
[ -d "$ROOT/scripts" ] && [ -d "$ROOT/packaging" ] || {
    echo "hamn: required Hamn release support files are missing" >&2
    exit 1
}

mkdir -p "$BINDIR" "$DATA_PARENT"
BINDIR=$(cd "$BINDIR" && pwd -P)
DATA_PARENT=$(cd "$DATA_PARENT" && pwd -P)
DATADIR=$DATA_PARENT/$DATA_BASE

path_contains() {
    local container=$1
    local candidate=$2
    if [ "$container" = / ]; then
        return 0
    fi
    case "$candidate/" in
    "$container/"*) return 0 ;;
    *) return 1 ;;
    esac
}

if path_contains "$BINDIR" "$DATADIR" ||
    path_contains "$DATADIR" "$BINDIR"; then
    echo "hamn: refusing overlapping binary and data directories: $BINDIR and $DATADIR" >&2
    exit 1
fi

source "$ROOT/scripts/install-transaction.sh"

HAMN_PATH=$BINDIR/hamn
LEGACY_BINARY_MARKER=$BINDIR/.hamn-binary.sha256
DATA_MARKER=$DATADIR/.hamn-managed
GENERATIONS=$DATADIR/.hamn-generations
INSTALL_UID=$(/usr/bin/id -u)

safe_owned_directory() {
    local path=$1
    [ -d "$path" ] && [ ! -L "$path" ] || return 1
    case "$(stat -f '%u:%Lp' "$path")" in
    "$INSTALL_UID:700"|"$INSTALL_UID:755") return 0 ;;
    *) return 1 ;;
    esac
}

safe_owned_directory "$BINDIR" && safe_owned_directory "$DATA_PARENT" || {
    echo "hamn: refusing writable or foreign install parent directory" >&2
    exit 1
}

path_absent() {
    [ ! -e "$1" ] && [ ! -L "$1" ]
}

path_identity() {
    local output
    output=$(install_support path-hash "$1") || return 1
    output=${output%% *}
    [[ "$output" =~ ^[0-9a-f]{64}$ ]] || return 1
    printf '%s\n' "$output"
}

BINDIR_ID=$(path_identity "$BINDIR")
DATADIR_ID=$(path_identity "$DATADIR")

# Serialize on both externally visible target roots. Lock files are permanent;
# their path, inode, owner, mode, and link count are rechecked after open/lock.
BIN_LOCK=$BINDIR/.hamn-install.lock
DATA_LOCK=$DATA_PARENT/.${DATA_BASE}.hamn-install.lock
if [[ "$BIN_LOCK" < "$DATA_LOCK" ]]; then
    LOCK_ONE=$BIN_LOCK
    LOCK_TWO=$DATA_LOCK
else
    LOCK_ONE=$DATA_LOCK
    LOCK_TWO=$BIN_LOCK
fi

lock_path_valid() {
    local lock_path=$1
    [ -f "$lock_path" ] && [ ! -L "$lock_path" ] || return 1
    [ "$(stat -f '%u:%Lp:%l' "$lock_path")" = \
        "$INSTALL_UID:600:1" ]
}

lock_path_prepare() {
    local lock_path=$1
    local old_umask
    if path_absent "$lock_path"; then
        old_umask=$(umask)
        umask 077
        set -C
        : >"$lock_path" 2>/dev/null || true
        set +C
        umask "$old_umask"
    fi
    if ! lock_path_valid "$lock_path"; then
        echo "hamn: refusing unsafe install lock path: $lock_path" >&2
        return 1
    fi
}

lock_path_identity() {
    stat -f '%d:%i:%u:%Lp:%l' "$1"
}

lock_fd_identity() {
    install_support fd-identity "$1"
}

lock_fd_exclusive() {
    install_support fd-lock "$1"
}

lock_path_prepare "$LOCK_ONE"
lock_one_identity=$(lock_path_identity "$LOCK_ONE")
exec 8>>"$LOCK_ONE"
lock_path_valid "$LOCK_ONE" &&
    [ "$(lock_path_identity "$LOCK_ONE")" = "$lock_one_identity" ] &&
    [ "$(lock_fd_identity 8)" = "$lock_one_identity" ] || {
    echo "hamn: install lock path changed while opening: $LOCK_ONE" >&2
    exit 1
}
lock_fd_exclusive 8 || {
    echo "hamn: failed to acquire install lock: $LOCK_ONE" >&2
    exit 1
}
lock_path_valid "$LOCK_ONE" &&
    [ "$(lock_path_identity "$LOCK_ONE")" = "$lock_one_identity" ] &&
    [ "$(lock_fd_identity 8)" = "$lock_one_identity" ] || {
    echo "hamn: install lock path changed while locking: $LOCK_ONE" >&2
    exit 1
}
if [ "$LOCK_TWO" != "$LOCK_ONE" ]; then
    lock_path_prepare "$LOCK_TWO"
    lock_two_identity=$(lock_path_identity "$LOCK_TWO")
    exec 9>>"$LOCK_TWO"
    lock_path_valid "$LOCK_TWO" &&
        [ "$(lock_path_identity "$LOCK_TWO")" = \
            "$lock_two_identity" ] &&
        [ "$(lock_fd_identity 9)" = "$lock_two_identity" ] || {
        echo "hamn: install lock path changed while opening: $LOCK_TWO" >&2
        exit 1
    }
    lock_fd_exclusive 9 || {
        echo "hamn: failed to acquire install lock: $LOCK_TWO" >&2
        exit 1
    }
    lock_path_valid "$LOCK_TWO" &&
        [ "$(lock_path_identity "$LOCK_TWO")" = \
            "$lock_two_identity" ] &&
        [ "$(lock_fd_identity 9)" = "$lock_two_identity" ] || {
        echo "hamn: install lock path changed while locking: $LOCK_TWO" >&2
        exit 1
    }
fi

owned_regular() {
    local path=$1
    local mode=$2
    [ -f "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(stat -f '%u:%Lp:%l' "$path")" = \
        "$INSTALL_UID:$mode:1" ]
}

owned_executable() {
    owned_regular "$1" 755 && [ -x "$1" ]
}

file_hash() {
    local output
    output=$(install_support hash "$1") || return 1
    output=${output%% *}
    [[ "$output" =~ ^[0-9a-f]{64}$ ]] || return 1
    printf '%s\n' "$output"
}

legacy_binary_marker_valid() {
    local expected_hash=$1
    local algorithm=
    local marker_hash=
    local extra=
    owned_regular "$LEGACY_BINARY_MARKER" 644 || return 1
    IFS=' ' read -r algorithm marker_hash extra \
        <"$LEGACY_BINARY_MARKER" || return 1
    [ "$algorithm" = sha256 ] && [ "$marker_hash" = "$expected_hash" ] &&
        [ -z "$extra" ]
}

data_marker_valid() {
    local marker_text
    [ -f "$DATA_MARKER" ] && [ ! -L "$DATA_MARKER" ] || return 1
    [ "$(stat -f '%u:%l' "$DATA_MARKER")" = "$INSTALL_UID:1" ] ||
        return 1
    case "$(stat -f '%Lp' "$DATA_MARKER")" in
    600|644) ;;
    *) return 1 ;;
    esac
    marker_text=$(<"$DATA_MARKER")
    [ -z "$marker_text" ] || [ "$marker_text" = version=1 ]
}

generation_marker_valid() {
    local generation=$1
    local expected_hash=$2
    local marker=$generation/.hamn-generation
    local line
    local version=
    local binary_hash=
    local marker_bindir_id=
    local marker_datadir_id=
    owned_regular "$marker" 600 || return 1
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
        version=*)
            [ -z "$version" ] || return 1
            version=${line#version=}
            ;;
        binary_sha256=*)
            [ -z "$binary_hash" ] || return 1
            binary_hash=${line#binary_sha256=}
            ;;
        bindir_id=*)
            [ -z "$marker_bindir_id" ] || return 1
            marker_bindir_id=${line#bindir_id=}
            ;;
        datadir_id=*)
            [ -z "$marker_datadir_id" ] || return 1
            marker_datadir_id=${line#datadir_id=}
            ;;
        *) return 1 ;;
        esac
    done <"$marker"
    [ "$version" = 1 ] && [ "$binary_hash" = "$expected_hash" ] &&
        [ "$marker_bindir_id" = "$BINDIR_ID" ] &&
        [ "$marker_datadir_id" = "$DATADIR_ID" ]
}

generation_valid() {
    local generation=$1
    local expected_hash=$2
    [ -d "$generation" ] && [ ! -L "$generation" ] || return 1
    [ "$(stat -f '%u:%Lp' "$generation")" = \
        "$INSTALL_UID:755" ] || return 1
    owned_executable "$generation/bin/hamn" || return 1
    [ "$(file_hash "$generation/bin/hamn")" = "$expected_hash" ] ||
        return 1
    [ -d "$generation/share/hamn/src/scripts" ] &&
        [ ! -L "$generation/share/hamn/src/scripts" ] &&
        [ -d "$generation/share/hamn/src/packaging" ] &&
        [ ! -L "$generation/share/hamn/src/packaging" ] &&
        [ -x "$generation/share/hamn/src/scripts/update-host.sh" ] ||
        return 1
    generation_marker_valid "$generation" "$expected_hash"
}

managed_hamn_link_valid() {
    [ -L "$HAMN_PATH" ] || return 1
    local target
    local relative
    local generation_name
    local generation_hash
    target=$(readlink "$HAMN_PATH") || return 1
    case "$target" in
    "$GENERATIONS/"*) ;;
    *) return 1 ;;
    esac
    relative=${target#"$GENERATIONS/"}
    generation_name=${relative%%/*}
    [ "$relative" = "$generation_name/bin/hamn" ] || return 1
    [[ "$generation_name" =~ ^[0-9a-f]{64}-[A-Za-z0-9]{6}$ ]] ||
        return 1
    generation_hash=${generation_name%%-*}
    generation_valid "$GENERATIONS/$generation_name" "$generation_hash"
}

if path_absent "$DATADIR"; then
    data_state=absent
elif [ -L "$DATADIR" ] || [ ! -d "$DATADIR" ]; then
    echo "hamn: refusing non-directory data path: $DATADIR" >&2
    exit 1
elif [ -e "$DATA_MARKER" ] || [ -L "$DATA_MARKER" ]; then
    data_marker_valid || {
        echo "hamn: refusing invalid data management marker: $DATA_MARKER" >&2
        exit 1
    }
    data_state=managed
elif [ -z "$(find "$DATADIR" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
    data_state=empty
else
    echo "hamn: refusing to modify unmanaged data directory: $DATADIR" >&2
    exit 1
fi
if [ "$data_state" != absent ] &&
    [ "$(stat -f '%u:%Lp' "$DATADIR")" != "$INSTALL_UID:755" ]; then
    echo "hamn: refusing writable or foreign data directory: $DATADIR" >&2
    exit 1
fi

hamn_kind=
hamn_original_identity=
hamn_original_target=
hamn_original_hash=
if path_absent "$HAMN_PATH"; then
    path_absent "$LEGACY_BINARY_MARKER" || {
        echo "hamn: refusing legacy marker without hamn executable" >&2
        exit 1
    }
    hamn_kind=absent
elif [ -L "$HAMN_PATH" ]; then
    managed_hamn_link_valid || {
        echo "hamn: refusing foreign hamn symlink: $HAMN_PATH" >&2
        exit 1
    }
    [ "$data_state" = managed ] || {
        echo "hamn: managed hamn symlink has no data ownership marker" >&2
        exit 1
    }
    hamn_kind=managed
    hamn_original_target=$(readlink "$HAMN_PATH")
    hamn_original_identity=$(stat -f '%d:%i:%u:%Lp:%l' "$HAMN_PATH")
elif owned_executable "$HAMN_PATH"; then
    [ "$data_state" = managed ] || {
        echo "hamn: legacy adoption requires a managed data marker" >&2
        exit 1
    }
    hamn_original_hash=$(file_hash "$HAMN_PATH")
    if path_absent "$LEGACY_BINARY_MARKER"; then
        [ "${HAMN_ADOPT_LEGACY:-0}" = 1 ] || {
            echo "hamn: refusing unmarked legacy executable; set HAMN_ADOPT_LEGACY=1 to adopt it" >&2
            exit 1
        }
    elif ! legacy_binary_marker_valid "$hamn_original_hash"; then
        echo "hamn: refusing legacy executable with invalid identity marker" >&2
        exit 1
    fi
    hamn_kind=legacy
    hamn_original_identity=$(stat -f '%d:%i:%u:%Lp:%l' "$HAMN_PATH")
else
    echo "hamn: refusing to replace foreign hamn executable: $HAMN_PATH" >&2
    exit 1
fi

validate_update_journal() {
    local cache state bootstrap selection attempt expected_count name
    [ -n "$UPDATE_JOURNAL" ] || return 0
    # The fourth argument is an internal handoff, never an arbitrary output.
    # The updater owns this HOME's journal and inherited cache-lock descriptor.
    cache=${HOME:?HOME is required}/.hamn/cache
    [ "$UPDATE_JOURNAL" = "$cache/.hamn-update-transaction" ] || return 1
    safe_owned_directory "$HOME/.hamn" &&
        [ "$(stat -f '%Lp' "$HOME/.hamn")" = 700 ] &&
        safe_owned_directory "$cache" &&
        safe_owned_directory "$UPDATE_JOURNAL" &&
        [ "$(stat -f '%Lp' "$UPDATE_JOURNAL")" = 700 ] || return 1
    install_support lock-same "$cache/.hamn-upgrade.lock" 5 || return 1
    for name in state attempt new-selection new-target; do
        owned_regular "$UPDATE_JOURNAL/$name" 600 || return 1
    done
    [ ! -s "$UPDATE_JOURNAL/new-target" ] || return 1
    attempt=$(cat "$UPDATE_JOURNAL/attempt") || return 1
    [[ "$attempt" =~ ^[A-Za-z0-9]{6}$ ]] || return 1
    state=$(cat "$UPDATE_JOURNAL/state") || return 1
    case "$state" in
    $'version=3\nbootstrap=0\nselection=present\nhostMutation=1') bootstrap=0; selection=present ;;
    $'version=3\nbootstrap=0\nselection=absent\nhostMutation=1') bootstrap=0; selection=absent ;;
    $'version=3\nbootstrap=1\nselection=present\nhostMutation=1') bootstrap=1; selection=present ;;
    $'version=3\nbootstrap=1\nselection=absent\nhostMutation=1') bootstrap=1; selection=absent ;;
    *) return 1 ;;
    esac
    expected_count=4
    if [ "$bootstrap" = 0 ]; then
        [ "$hamn_kind" = managed ] &&
            owned_regular "$UPDATE_JOURNAL/old-target" 600 &&
            [ "$(cat "$UPDATE_JOURNAL/old-target")" = "$hamn_original_target" ] || return 1
        expected_count=$((expected_count + 1))
    else
        [ "$hamn_kind" = absent ] || return 1
    fi
    if [ "$selection" = present ]; then
        owned_regular "$UPDATE_JOURNAL/previous-selection" 600 || return 1
        expected_count=$((expected_count + 1))
    fi
    [ "$(find "$UPDATE_JOURNAL" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" = "$expected_count" ]
}
validate_update_journal || {
    echo "hamn: unsafe update journal handoff" >&2
    exit 1
}

create_data_marker() {
    local marker_stage
    marker_stage=$(/usr/bin/mktemp -d \
        "$DATA_PARENT/.${DATA_BASE}.hamn-marker.XXXXXX")
    [ "$(stat -f '%u:%Lp' "$marker_stage")" = "$INSTALL_UID:700" ] ||
        return 1
    printf 'version=1\n' >"$marker_stage/marker"
    chmod 0600 "$marker_stage/marker"
    /bin/sync
    /bin/mv -n "$marker_stage/marker" "$DATA_MARKER"
    data_marker_valid || return 1
    /bin/rmdir "$marker_stage"
    /bin/sync
}

# Early Hamn installs used an empty marker.  It is sufficient to establish
# ownership for this one-way installer migration, but the signed updater
# requires an explicit schema version before it may change either public
# pointer.  Upgrade only after every existing binary/link ownership check has
# passed, and publish the new marker atomically.
upgrade_legacy_data_marker() {
    local marker_stage
    data_marker_valid && [ -z "$(<"$DATA_MARKER")" ] || return 1
    marker_stage=$(/usr/bin/mktemp -d \
        "$DATA_PARENT/.${DATA_BASE}.hamn-marker-upgrade.XXXXXX")
    [ "$(stat -f '%u:%Lp' "$marker_stage")" = "$INSTALL_UID:700" ] ||
        return 1
    printf 'version=1\n' >"$marker_stage/marker"
    chmod 0600 "$marker_stage/marker"
    /bin/sync
    data_marker_valid && [ -z "$(<"$DATA_MARKER")" ] || return 1
    /bin/mv -f "$marker_stage/marker" "$DATA_MARKER"
    data_marker_valid && [ "$(<"$DATA_MARKER")" = version=1 ] || return 1
    /bin/rmdir "$marker_stage"
    /bin/sync
}

if [ "$data_state" = absent ]; then
    mkdir "$DATADIR"
    chmod 0755 "$DATADIR"
fi
if [ "$data_state" = absent ] || [ "$data_state" = empty ]; then
    create_data_marker
elif [ -z "$(<"$DATA_MARKER")" ]; then
    upgrade_legacy_data_marker || {
        echo "hamn: cannot upgrade the legacy data management marker" >&2
        exit 1
    }
fi
[ -d "$DATADIR" ] && [ ! -L "$DATADIR" ] &&
    [ "$(stat -f '%u:%Lp' "$DATADIR")" = "$INSTALL_UID:755" ] &&
    data_marker_valid && [ "$(<"$DATA_MARKER")" = version=1 ] || {
    echo "hamn: data directory ownership changed during install" >&2
    exit 1
}

if path_absent "$GENERATIONS"; then
    mkdir "$GENERATIONS"
    chmod 0755 "$GENERATIONS"
fi
[ -d "$GENERATIONS" ] && [ ! -L "$GENERATIONS" ] &&
    [ "$(stat -f '%u:%Lp' "$GENERATIONS")" = \
        "$INSTALL_UID:755" ] || {
    echo "hamn: refusing unsafe generation root: $GENERATIONS" >&2
    exit 1
}

new_hash=$(file_hash "$SOURCE")
generation_stage=$(/usr/bin/mktemp -d "$GENERATIONS/.staging.XXXXXX")
generation_suffix=${generation_stage##*.staging.}
[[ "$generation_suffix" =~ ^[A-Za-z0-9]{6}$ ]] || exit 1
generation=$GENERATIONS/$new_hash-$generation_suffix
path_absent "$generation" || {
    echo "hamn: generated install identity already exists: $generation" >&2
    exit 1
}

mkdir -p "$generation_stage/bin" \
    "$generation_stage/share/hamn/src"
install -m 0755 "$SOURCE" "$generation_stage/bin/hamn"
[ "$(file_hash "$generation_stage/bin/hamn")" = "$new_hash" ] || {
    echo "hamn: staged binary differs from install source" >&2
    exit 1
}
rsync -a --delete --exclude build \
    "$ROOT/scripts" "$ROOT/packaging" \
    "$generation_stage/share/hamn/src/"
chmod 0755 "$generation_stage"
# Older generations cannot enumerate recovery roots from other HOME values.
# Only generations created with the retention contract opt into collection.
printf 'version=1\n' >"$generation_stage/.hamn-retention"
chmod 0600 "$generation_stage/.hamn-retention"
if [ -n "$hamn_original_target" ]; then
    printf '%s\n' "$hamn_original_target" >"$generation_stage/.hamn-previous-target"
    chmod 0600 "$generation_stage/.hamn-previous-target"
fi
# Marker-last: only a fully copied and hash-verified generation is publishable.
/bin/sync
{
    printf 'version=1\n'
    printf 'binary_sha256=%s\n' "$new_hash"
    printf 'bindir_id=%s\n' "$BINDIR_ID"
    printf 'datadir_id=%s\n' "$DATADIR_ID"
} >"$generation_stage/.hamn-generation"
chmod 0600 "$generation_stage/.hamn-generation"
/bin/sync
generation_valid "$generation_stage" "$new_hash" || {
    echo "hamn: staged generation validation failed" >&2
    exit 1
}
/bin/mv "$generation_stage" "$generation"
/bin/sync
generation_valid "$generation" "$new_hash" || {
    echo "hamn: published generation validation failed" >&2
    exit 1
}

if [ -n "$UPDATE_JOURNAL" ]; then
    validate_update_journal || {
        echo "hamn: update journal changed before generation publication" >&2
        exit 1
    }
    # Recording after symlink publication would leave an unowned rollback
    # window on SIGKILL. Persist the exact target before exposing that link.
    install_support recovery-root "$generation/bin/hamn" "${UPDATE_JOURNAL%/*}"
    target_stage=$(/usr/bin/mktemp "${UPDATE_JOURNAL%/*}/.hamn-target.XXXXXX")
    printf '%s\n' "$generation/bin/hamn" >"$target_stage"
    chmod 0600 "$target_stage"
    /bin/sync
    /bin/mv -f "$target_stage" "$UPDATE_JOURNAL/new-target"
    /bin/sync
    owned_regular "$UPDATE_JOURNAL/new-target" 600 &&
        [ "$(cat "$UPDATE_JOURNAL/new-target")" = "$generation/bin/hamn" ] || exit 1
fi

make_link_stage() {
    local prefix=$1
    local target=$2
    local link_stage
    link_stage=$(/usr/bin/mktemp -d "$BINDIR/$prefix.XXXXXX")
    [ "$(stat -f '%u:%Lp' "$link_stage")" = "$INSTALL_UID:700" ] ||
        return 1
    ln -s "$target" "$link_stage/link"
    [ "$(readlink "$link_stage/link")" = "$target" ] || return 1
    printf '%s\n' "$link_stage"
}

hamn_link_stage=$(make_link_stage .hamn-link "$generation/bin/hamn")
hamn_link_temp=$hamn_link_stage/link
case "$hamn_kind" in
absent)
    path_absent "$HAMN_PATH" && path_absent "$LEGACY_BINARY_MARKER" || {
        echo "hamn: hamn path changed before commit" >&2
        exit 1
    }
    /bin/mv -n "$hamn_link_temp" "$HAMN_PATH"
    ;;
managed)
    managed_hamn_link_valid &&
        [ "$(readlink "$HAMN_PATH")" = "$hamn_original_target" ] &&
        [ "$(stat -f '%d:%i:%u:%Lp:%l' "$HAMN_PATH")" = \
            "$hamn_original_identity" ] || {
        echo "hamn: managed hamn path changed before commit" >&2
        exit 1
    }
    /bin/mv -f "$hamn_link_temp" "$HAMN_PATH"
    ;;
legacy)
    owned_executable "$HAMN_PATH" &&
        [ "$(file_hash "$HAMN_PATH")" = "$hamn_original_hash" ] &&
        [ "$(stat -f '%d:%i:%u:%Lp:%l' "$HAMN_PATH")" = \
            "$hamn_original_identity" ] || {
        echo "hamn: legacy hamn path changed before commit" >&2
        exit 1
    }
    /bin/mv -f "$hamn_link_temp" "$HAMN_PATH"
    ;;
esac
/bin/sync
[ -L "$HAMN_PATH" ] &&
    [ "$(readlink "$HAMN_PATH")" = "$generation/bin/hamn" ] &&
    managed_hamn_link_valid || {
    echo "hamn: committed generation validation failed" >&2
    exit 1
}
/bin/rmdir "$hamn_link_stage"

# Collection is best effort after the durable public commit. Never roll back a
# successful installation because an obsolete generation could not be removed.
install_support prune "$BINDIR" "$DATADIR" \
    "$hamn_original_target" "$ROOT" ||
    echo "hamn: obsolete generation cleanup deferred" >&2

echo "installed: $HAMN_PATH -> $generation/bin/hamn"
echo "installed: $generation/share/hamn/src/{scripts,packaging}"
echo "verify: hamn --headless vm start --profile default --yes"
echo "Docker connection: hamn --headless vm env --profile default"
