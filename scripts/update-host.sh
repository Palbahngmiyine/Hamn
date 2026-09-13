#!/bin/bash
# Install one signed, compatible Hamn release without rebuilding it locally.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn update: $*" >&2
    exit 1
}

# Human progress goes to stderr; stdout belongs to the headless JSON protocol.
# HAMN_UPDATE_PROGRESS is set by the frontend because the worker uses a pipe.
progress() {
    echo "hamn update: $*" >&2
}

usage() {
    echo "usage: update-host.sh --bindir DIR --datadir DIR [--manifest URL_OR_PATH] [--bootstrap]" >&2
    exit 2
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

safe_directory() {
    local path=$1
    [ -d "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(stat -f '%u:%Lp' "$path")" = "$(id -u):755" ] || return 1
}

safe_regular() {
    local path=$1
    [ -f "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(stat -f '%u:%l' "$path")" = "$(id -u):1" ]
}

safe_private_directory() {
    local path=$1
    [ -d "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(stat -f '%u:%Lp' "$path")" = "$(id -u):700" ]
}

safe_private_regular() {
    local path=$1
    [ -f "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(stat -f '%u:%Lp:%l' "$path")" = "$(id -u):600:1" ]
}

path_absent() {
    [ ! -e "$1" ] && [ ! -L "$1" ]
}

fetch() {
    local source=$1 destination=$2
    local curl_output=(--silent)
    if [ -t 2 ] || [ "${HAMN_UPDATE_PROGRESS:-0}" = 1 ]; then
        curl_output=(--progress-bar)
    fi
    case "$source" in
    https://*)
        curl --fail --show-error --location "${curl_output[@]}" --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 \
            -o "$destination" "$source"
        ;;
    file://*)
        [ "${HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS:-0}" = 1 ] ||
            fail "local artifacts are disabled"
        local path=${source#file://}
        safe_regular "$path" || fail "unsafe local artifact: $path"
        cp "$path" "$destination"
        ;;
    /*)
        [ "${HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS:-0}" = 1 ] ||
            fail "local artifacts are disabled"
        safe_regular "$source" || fail "unsafe local artifact: $source"
        cp "$source" "$destination"
        ;;
    *)
        fail "artifact URL must use HTTPS"
        ;;
    esac
}

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
source_root=$(cd "$script_dir/.." && pwd -P)
bindir=
datadir=
manifest_ref=
bootstrap=0
while [ "$#" -gt 0 ]; do
    case "$1" in
    --bindir)
        [ "$#" -ge 2 ] && [ -z "$bindir" ] || usage
        bindir=$2
        shift 2
        ;;
    --datadir)
        [ "$#" -ge 2 ] && [ -z "$datadir" ] || usage
        datadir=$2
        shift 2
        ;;
    --manifest)
        [ "$#" -ge 2 ] && [ -z "$manifest_ref" ] || usage
        manifest_ref=$2
        shift 2
        ;;
    --bootstrap)
        [ "$bootstrap" = 0 ] || usage
        bootstrap=1
        shift
        ;;
    *) usage ;;
    esac
done
[ -n "$bindir" ] && [ -n "$datadir" ] || usage

BINDIR=$bindir DATADIR=$datadir
source "$script_dir/install-transaction.sh"
bindir=$BINDIR datadir=$DATADIR
prune_generations() {
    python3 "$script_dir/prune-generations.py" "$bindir" "$datadir" \
        "$old_target" "$source_root" ||
        echo "hamn update: obsolete generation cleanup deferred" >&2
}

hamn_link=$bindir/hamn
bootstrap_entry=$bootstrap
old_target=
# A bootstrap over a managed installation has the same rollback obligations as
# an update. Do not discard its old target just because the entrypoint changed.
if [ "$bootstrap" = 1 ] && ! path_absent "$hamn_link"; then
    [ -L "$hamn_link" ] ||
        fail "existing hamn is not a managed generation; migrate it with make install before using the release installer"
    bootstrap=0
fi
if [ "$bootstrap" = 0 ]; then
    safe_directory "$bindir" || fail "unsafe managed binary directory: $bindir"
    safe_directory "$datadir" || fail "unsafe managed data directory: $datadir"
    safe_regular "$datadir/.hamn-managed" ||
        fail "managed data marker is missing or unsafe"
    managed_marker=$(cat "$datadir/.hamn-managed")
    if [ "$managed_marker" != version=1 ]; then
        # The installer can upgrade the original empty ownership marker after
        # validating the existing generation. Preserve that bootstrap migration.
        [ "$bootstrap_entry" = 1 ] && [ -z "$managed_marker" ] ||
            fail "managed data marker is invalid"
    fi
    # install-host publishes canonical absolute targets. Resolve parent aliases
    # only after rejecting unsafe leaf directories (for example /tmp on macOS).
    bindir=$(cd "$bindir" && pwd -P)
    datadir=$(cd "$datadir" && pwd -P)
    hamn_link=$bindir/hamn
    [ -L "$hamn_link" ] || fail "managed hamn link is missing"
    old_target=$(readlink "$hamn_link") || fail "cannot read managed hamn link"
    case "$old_target" in
    "$datadir/.hamn-generations/"*/bin/hamn) ;;
    *) fail "managed hamn link points outside its generation root" ;;
    esac
fi

[ -n "${HOME:-}" ] || fail "HOME is not set"
runtime_root=$HOME/.hamn
if [ ! -d "$runtime_root" ]; then
    mkdir -m 0700 "$runtime_root"
fi
[ -d "$runtime_root" ] && [ ! -L "$runtime_root" ] ||
    fail "unsafe Hamn runtime root"
cache=$runtime_root/cache
if [ ! -d "$cache" ]; then
    mkdir -m 0755 "$cache"
fi
safe_directory "$cache" || fail "unsafe Hamn image cache"

guest_selection=$cache/guest-image.json
update_journal=$cache/.hamn-update-transaction
journal_directory=$update_journal
journal_bootstrap=
journal_selection_state=
journal_old_target=
journal_attempt=
journal_stage=

managed_generation_target() {
    local target=$1 relative generation_root
    # A fresh bootstrap creates these directories only during host install.
    # Its supplied path may still contain an alias such as macOS /tmp.
    safe_directory "$datadir" || return 1
    generation_root=$(cd "$datadir" && pwd -P) || return 1
    generation_root=$generation_root/.hamn-generations
    case "$target" in
    "$generation_root/"*) ;;
    *) return 1 ;;
    esac
    relative=${target#"$generation_root/"}
    [[ "$relative" =~ ^[0-9a-f]{64}-[A-Za-z0-9]{6}/bin/hamn$ ]] ||
        return 1
    safe_regular "$target"
}

journal_entry_count() {
    find "$journal_directory" -mindepth 1 -maxdepth 1 -print |
        wc -l | tr -d ' '
}

load_update_journal() {
    local state attempt expected_count
    safe_private_directory "$journal_directory" || return 1
    safe_private_regular "$journal_directory/state" || return 1
    safe_private_regular "$journal_directory/attempt" || return 1
    safe_private_regular "$journal_directory/new-selection" || return 1
    state=$(cat "$journal_directory/state") || return 1
    attempt=$(cat "$journal_directory/attempt") || return 1
    [[ "$attempt" =~ ^[A-Za-z0-9]{6}$ ]] || return 1
    case "$state" in
    $'version=1\nbootstrap=0\nselection=present')
        journal_bootstrap=0
        journal_selection_state=present
        expected_count=5
        ;;
    $'version=1\nbootstrap=0\nselection=absent')
        journal_bootstrap=0
        journal_selection_state=absent
        expected_count=4
        ;;
    $'version=1\nbootstrap=1\nselection=present')
        journal_bootstrap=1
        journal_selection_state=present
        expected_count=4
        ;;
    $'version=1\nbootstrap=1\nselection=absent')
        journal_bootstrap=1
        journal_selection_state=absent
        expected_count=3
        ;;
    *) return 1 ;;
    esac
    if [ "$journal_selection_state" = present ]; then
        safe_private_regular "$journal_directory/previous-selection" || return 1
    else
        path_absent "$journal_directory/previous-selection" || return 1
    fi
    if [ "$journal_bootstrap" = 0 ]; then
        safe_private_regular "$journal_directory/old-target" || return 1
        journal_old_target=$(cat "$journal_directory/old-target") || return 1
        [[ "$journal_old_target" != *$'\n'* ]] || return 1
        managed_generation_target "$journal_old_target" || return 1
    else
        path_absent "$journal_directory/old-target" || return 1
        journal_old_target=
    fi
    [ "$(journal_entry_count)" = "$expected_count" ] || return 1
    journal_attempt=$attempt
}

discard_journal_stage() {
    local stage=$1
    [ -n "$stage" ] || return 0
    safe_private_directory "$stage" || return 1
    rm -f "$stage/state" "$stage/attempt" "$stage/new-selection" \
        "$stage/previous-selection" "$stage/old-target"
    rmdir "$stage"
}

retire_update_journal() {
    local outcome=$1 retired
    journal_directory=$update_journal
    load_update_journal || return 1
    case "$outcome" in
    completed|recovered) ;;
    *) return 1 ;;
    esac
    retired=$cache/.hamn-update-$outcome.$journal_attempt
    path_absent "$retired" || return 1
    mv "$update_journal" "$retired" || return 1
    path_absent "$update_journal" || return 1
    /bin/sync ||
        echo "hamn update: transaction retirement is pending a filesystem flush" >&2
}

cleanup_deferred_journal() {
    local deferred=$1 entry name
    safe_private_directory "$deferred" || return 1
    while IFS= read -r -d '' entry; do
        name=${entry##*/}
        case "$name" in
        state|attempt|new-selection|previous-selection|old-target)
            safe_private_regular "$entry" || return 1
            ;;
        *) return 1 ;;
        esac
    done < <(find "$deferred" -mindepth 1 -maxdepth 1 -print0)
    rm -f "$deferred/state" "$deferred/attempt" "$deferred/new-selection" \
        "$deferred/previous-selection" "$deferred/old-target" || return 1
    rmdir "$deferred"
}

cleanup_deferred_journals() {
    local deferred name
    for deferred in "$cache"/.hamn-update-cleanup.*.*; do
        [ -e "$deferred" ] || [ -L "$deferred" ] || continue
        name=${deferred##*/}
        [[ "$name" =~ ^\.hamn-update-cleanup\.(completed|recovered)\.[A-Za-z0-9]{6}$ ]] ||
            return 1
        cleanup_deferred_journal "$deferred" || return 1
    done
}

cleanup_retired_journal() {
    local retired=$1 previous_directory=$journal_directory name outcome attempt deferred
    journal_directory=$retired
    if ! load_update_journal; then
        journal_directory=$previous_directory
        return 1
    fi
    name=${retired##*/}
    if [[ "$name" =~ ^\.hamn-update-(completed|recovered)\.([A-Za-z0-9]{6})$ ]]; then
        outcome=${BASH_REMATCH[1]}
        attempt=${BASH_REMATCH[2]}
    else
        journal_directory=$previous_directory
        return 1
    fi
    deferred=$cache/.hamn-update-cleanup.$outcome.$attempt
    if ! path_absent "$deferred" || ! mv "$retired" "$deferred"; then
        journal_directory=$previous_directory
        return 1
    fi
    /bin/sync ||
        echo "hamn update: retired transaction cleanup is pending a filesystem flush" >&2
    journal_directory=$previous_directory
    cleanup_deferred_journal "$deferred"
}

cleanup_retired_journals() {
    local retired name
    for retired in "$cache"/.hamn-update-completed.* \
        "$cache"/.hamn-update-recovered.*; do
        [ -e "$retired" ] || [ -L "$retired" ] || continue
        name=${retired##*/}
        [[ "$name" =~ ^\.hamn-update-(completed|recovered)\.[A-Za-z0-9]{6}$ ]] ||
            return 1
        cleanup_retired_journal "$retired" || return 1
    done
}

restore_guest_selection_from_journal() {
    local selection_stage
    if [ "$journal_selection_state" = present ]; then
        selection_stage=$(mktemp "$cache/.guest-image.json.rollback.XXXXXX") ||
            return 1
        if ! cp "$journal_directory/previous-selection" "$selection_stage" ||
            ! chmod 0600 "$selection_stage" ||
            ! cmp -s "$selection_stage" "$journal_directory/previous-selection"; then
            rm -f "$selection_stage"
            return 1
        fi
        mv -f "$selection_stage" "$guest_selection" || return 1
        /bin/sync
        safe_regular "$guest_selection" &&
            cmp -s "$guest_selection" "$journal_directory/previous-selection"
        return
    fi
    if ! path_absent "$guest_selection"; then
        safe_regular "$guest_selection" || return 1
        rm -f "$guest_selection"
        /bin/sync
    fi
    path_absent "$guest_selection"
}

restore_binary_link_from_journal() {
    local current_target link_stage
    [ "$journal_bootstrap" = 0 ] || return 0
    managed_generation_target "$journal_old_target" || return 1
    [ -L "$hamn_link" ] || return 1
    current_target=$(readlink "$hamn_link") || return 1
    [ "$current_target" = "$journal_old_target" ] && return 0
    managed_generation_target "$current_target" || return 1
    link_stage=$(mktemp -d "$bindir/.hamn-update-rollback.XXXXXX") || return 1
    if ! ln -s "$journal_old_target" "$link_stage/hamn" ||
        ! mv -f "$link_stage/hamn" "$hamn_link" ||
        ! rmdir "$link_stage"; then
        rm -f "$link_stage/hamn"
        rmdir "$link_stage" 2>/dev/null || true
        return 1
    fi
    /bin/sync
    [ "$(readlink "$hamn_link")" = "$journal_old_target" ]
}

rollback_update_journal() {
    journal_directory=$update_journal
    load_update_journal || return 1
    restore_guest_selection_from_journal || return 1
    restore_binary_link_from_journal || return 1
    recovery_summary="prior binary and guest image selection were restored"
    if [ "$journal_bootstrap" = 1 ]; then
        recovery_summary="guest image selection was restored; no previous binary was recorded, so a published command may remain"
    fi
    retire_update_journal recovered
}

recover_pending_update() {
    path_absent "$update_journal" && return 0
    if ! rollback_update_journal; then
        echo "hamn update: incomplete prior update could not be safely recovered" >&2
        return 1
    fi
    if [ "$journal_bootstrap" = 0 ]; then
        echo "hamn update: recovered the previous binary and guest image selection after an interrupted update" >&2
    else
        progress "Recovered interrupted bootstrap: $recovery_summary"
    fi
}

prepare_update_journal() {
    local new_selection=$1 selection_state expected_attempt
    journal_directory=$update_journal
    path_absent "$update_journal" || {
        echo "hamn update: another update transaction is already active" >&2
        return 1
    }
    journal_stage=$(mktemp -d "$cache/.hamn-update-transaction.XXXXXX") ||
        return 1
    safe_private_directory "$journal_stage" || {
        discard_journal_stage "$journal_stage" || true
        journal_stage=
        return 1
    }
    journal_attempt=${journal_stage##*.hamn-update-transaction.}
    [[ "$journal_attempt" =~ ^[A-Za-z0-9]{6}$ ]] || {
        discard_journal_stage "$journal_stage" || true
        journal_stage=
        return 1
    }
    expected_attempt=$journal_attempt
    if path_absent "$guest_selection"; then
        selection_state=absent
    else
        safe_regular "$guest_selection" || {
            discard_journal_stage "$journal_stage" || true
            journal_stage=
            return 1
        }
        selection_state=present
        cp "$guest_selection" "$journal_stage/previous-selection" || {
            discard_journal_stage "$journal_stage" || true
            journal_stage=
            return 1
        }
        chmod 0600 "$journal_stage/previous-selection" || {
            discard_journal_stage "$journal_stage" || true
            journal_stage=
            return 1
        }
    fi
    if [ "$bootstrap" = 0 ]; then
        managed_generation_target "$old_target" || {
            discard_journal_stage "$journal_stage" || true
            journal_stage=
            return 1
        }
        # Remember every recovery root using this generation, including callers
        # using different HOME directories. Publish before the durable journal.
        if ! python3 - "$old_target" "$cache" <<'PY_ROOT'
import hashlib, os, pathlib, stat, sys, tempfile
root = pathlib.Path(sys.argv[1]).parent.parent
cache = str(pathlib.Path(sys.argv[2]).resolve())
p = root / ('.hamn-recovery-root-' + hashlib.sha256(cache.encode()).hexdigest())
fd, temporary = tempfile.mkstemp(prefix='.hamn-root.', dir=root)
try:
    with os.fdopen(fd, 'w') as out:
        out.write(cache)
        out.flush()
        os.fsync(out.fileno())
    if p.exists() or p.is_symlink():
        s = p.lstat()
        if not stat.S_ISREG(s.st_mode) or stat.S_IMODE(s.st_mode) != 0o600 or s.st_uid != os.getuid() or s.st_nlink != 1 or p.read_text() != cache:
            sys.exit('unsafe generation recovery root')
    else:
        # Both install roots are locked; rename leaves no hardlink window if killed.
        os.rename(temporary, p)
finally:
    pathlib.Path(temporary).unlink(missing_ok=True)
PY_ROOT
        then
            discard_journal_stage "$journal_stage" || true
            journal_stage=
            return 1
        fi
        printf '%s\n' "$old_target" >"$journal_stage/old-target"
        chmod 0600 "$journal_stage/old-target" || {
            discard_journal_stage "$journal_stage" || true
            journal_stage=
            return 1
        }
    fi
    cp "$new_selection" "$journal_stage/new-selection" || {
        discard_journal_stage "$journal_stage" || true
        journal_stage=
        return 1
    }
    chmod 0600 "$journal_stage/new-selection" || {
        discard_journal_stage "$journal_stage" || true
        journal_stage=
        return 1
    }
    {
        printf 'version=1\n'
        printf 'bootstrap=%s\n' "$bootstrap"
        printf 'selection=%s\n' "$selection_state"
    } >"$journal_stage/state"
    printf '%s\n' "$journal_attempt" >"$journal_stage/attempt"
    chmod 0600 "$journal_stage/state" "$journal_stage/attempt" || {
        discard_journal_stage "$journal_stage" || true
        journal_stage=
        return 1
    }
    /bin/sync
    if ! mv -n "$journal_stage" "$update_journal"; then
        discard_journal_stage "$journal_stage" || true
        journal_stage=
        return 1
    fi
    if ! load_update_journal || [ "$journal_attempt" != "$expected_attempt" ]; then
        if [ -d "$journal_stage" ]; then
            discard_journal_stage "$journal_stage" || true
        fi
        journal_stage=
        return 1
    fi
    journal_stage=
    /bin/sync
}

commit_guest_selection_from_journal() {
    local selection_stage
    journal_directory=$update_journal
    load_update_journal || return 1
    selection_stage=$(mktemp "$cache/.guest-image.json.update.XXXXXX") ||
        return 1
    if ! cp "$journal_directory/new-selection" "$selection_stage" ||
        ! chmod 0600 "$selection_stage" ||
        ! cmp -s "$selection_stage" "$journal_directory/new-selection"; then
        rm -f "$selection_stage"
        return 1
    fi
    mv -f "$selection_stage" "$guest_selection" || return 1
    /bin/sync
    safe_regular "$guest_selection" &&
        cmp -s "$guest_selection" "$journal_directory/new-selection"
}

test_after_host_install_barrier() {
    local ready=${HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_READY_FIFO:-}
    local release=${HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_RELEASE_FIFO:-}
    [ -z "$ready" ] && [ -z "$release" ] && return 0
    [ -n "$ready" ] && [ -n "$release" ] && [ -p "$ready" ] &&
        [ -p "$release" ] || return 1
    printf 'ready\n' >"$ready"
    IFS= read -r _ <"$release"
}

test_after_journal_retire_barrier() {
    local ready=${HAMN_TEST_UPDATE_AFTER_JOURNAL_RETIRE_READY_FIFO:-}
    local release=${HAMN_TEST_UPDATE_AFTER_JOURNAL_RETIRE_RELEASE_FIFO:-}
    [ -z "$ready" ] && [ -z "$release" ] && return 0
    [ -n "$ready" ] && [ -n "$release" ] && [ -p "$ready" ] &&
        [ -p "$release" ] || return 1
    printf 'ready\n' >"$ready"
    IFS= read -r _ <"$release"
}

interrupted_update() {
    local signal=${1:-TERM} status
    trap - HUP INT TERM
    if ! rollback_update_journal; then
        echo "hamn update: interrupted by $signal; recovery journal remains for a later safe recovery" >&2
    else
        echo "hamn update: interrupted by $signal; $recovery_summary" >&2
    fi
    case "$signal" in
    HUP) status=129 ;;
    INT) status=130 ;;
    TERM) status=143 ;;
    *) status=1 ;;
    esac
    exit "$status"
}

cleanup_deferred_journals ||
    fail "a deferred update transaction cleanup is unsafe or could not be cleaned"
cleanup_retired_journals ||
    fail "a retired update transaction is unsafe or could not be cleaned"
recover_pending_update || fail "previous update recovery failed; no new update was installed"
cleanup_retired_journals ||
    fail "the recovered update transaction could not be cleaned"
cleanup_deferred_journals ||
    fail "the recovered transaction cleanup is unsafe or could not be cleaned"

# Recovery may have restored a different generation. Snapshot the recovered
# target, not the interrupted candidate, for this attempt's rollback journal.
if [ "$bootstrap" = 0 ]; then
    old_target=$(readlink "$hamn_link") || fail "cannot read recovered hamn link"
    managed_generation_target "$old_target" || fail "recovered hamn link is unsafe"
fi

if [ -z "$manifest_ref" ]; then
    manifest_url_file=$source_root/packaging/release/update-manifest-url
    safe_regular "$manifest_url_file" ||
        fail "this build has no configured update manifest URL"
    manifest_ref=$(cat "$manifest_url_file")
fi
[ -n "$manifest_ref" ] || fail "manifest URL is empty"

work=$(mktemp -d "${TMPDIR:-/tmp}/hamn-update.XXXXXX")
cleanup() {
    rm -rf "$work"
}
trap cleanup EXIT
manifest=$work/manifest.json
progress "Checking release metadata..."
fetch "$manifest_ref" "$manifest" || fail "release metadata download failed; check your connection and retry"

if ! python3 - "$manifest" "$(sw_vers -productVersion)" "$(uname -m)" <<'PY' \
    >"$work/manifest-fields"
import json
import re
import sys


def pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key: " + key)
        result[key] = value
    return result


def version(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9]+(?:\.[0-9]+){0,2}", value):
        raise ValueError("invalid macOS version")
    return tuple(int(part) for part in value.split("."))


def require_keys(value, keys, label):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ValueError(label + " has an invalid schema")


def artifact(value, label):
    require_keys(value, ("url", "sha256"), label)
    url = value["url"]
    digest = value["sha256"]
    if not isinstance(url, str) or not url or any(ord(ch) < 33 or ord(ch) > 126 for ch in url):
        raise ValueError(label + " URL is invalid")
    if not re.fullmatch(r"[0-9a-f]{64}", digest if isinstance(digest, str) else ""):
        raise ValueError(label + " SHA-256 is invalid")
    return url, digest


try:
    with open(sys.argv[1], encoding="utf-8") as source:
        manifest = json.load(source, object_pairs_hook=pairs,
                             parse_constant=lambda value: (_ for _ in ()).throw(ValueError(value)))
    # v0.1.x published repository as optional descriptive metadata. Accept
    # that exact extension, while retaining strict rejection of unknown keys.
    if isinstance(manifest, dict) and "repository" in manifest:
        repository = manifest.pop("repository")
        if not isinstance(repository, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
            raise ValueError("release repository is invalid")
    require_keys(manifest, ("schemaVersion", "channel", "version", "commit",
                            "validationMode", "compatibility", "artifacts"),
                 "manifest")
    if manifest["schemaVersion"] != 2 or manifest["channel"] != "stable":
        raise ValueError("manifest is not a stable schema v2 release")
    if not isinstance(manifest["version"], str) or not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", manifest["version"]):
        raise ValueError("release version is invalid")
    if not isinstance(manifest["commit"], str) or \
            not re.fullmatch(r"[0-9a-f]{40}", manifest["commit"]):
        raise ValueError("release commit is invalid")
    if manifest["validationMode"] not in ("github-hosted-no-vm", "physical-apple-silicon"):
        raise ValueError("release validation mode is invalid")
    compatibility = manifest["compatibility"]
    require_keys(compatibility, ("os", "architecture", "minimumMacOS"), "compatibility")
    if compatibility["os"] != "darwin" or compatibility["architecture"] != "arm64":
        raise ValueError("manifest is not compatible with Apple Silicon macOS")
    current = version(sys.argv[2])
    minimum = version(compatibility["minimumMacOS"])
    current = current + (0,) * (3 - len(current))
    minimum = minimum + (0,) * (3 - len(minimum))
    if current < minimum:
        raise ValueError("macOS is below the release minimum")
    if sys.argv[3] not in ("arm64", "arm64e"):
        raise ValueError("host architecture is not Apple Silicon")
    artifacts = manifest["artifacts"]
    require_keys(artifacts, ("host", "guestImage"), "artifacts")
    host_url, host_hash = artifact(artifacts["host"], "host artifact")
    guest_url, guest_hash = artifact(artifacts["guestImage"], "guest image artifact")
except (OSError, TypeError, ValueError, json.JSONDecodeError) as error:
    raise SystemExit("hamn update: invalid immutable release manifest: " + str(error))

print(manifest["version"])
print(host_url)
print(host_hash)
print(guest_url)
print(guest_hash)
PY
then
    progress "No new release was installed. See the official installer recovery instructions:"
    progress "https://github.com/Palbahngmiyine/Hamn#install"
    exit 1
fi
{
    IFS= read -r release_version
    IFS= read -r host_url
    IFS= read -r host_hash
    IFS= read -r guest_url
    IFS= read -r guest_hash
} <"$work/manifest-fields"
[ -n "$host_url" ] && [ -n "$host_hash" ] && [ -n "$guest_url" ] &&
    [ -n "$guest_hash" ] || fail "immutable manifest fields are incomplete"

# A receipt is advisory: absent, malformed, unsafe, or stale evidence forces the
# normal verified install. It binds release identities to the installed files;
# a version string alone never permits skipping downloads. Each generation owns
# its private receipt, so rollback restores the previous receipt with the link.
release_receipt() {
    python3 - "$1" "$2" "$release_version" "$host_hash" "$guest_hash" "$cache" <<'PY_RECEIPT'
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import tempfile

mode, target, version, host_hash, guest_hash, cache = sys.argv[1:]
generation = Path(target).parent.parent
receipt = generation / ".hamn-release.json"


def owned(path, directory=False):
    info = path.lstat()
    valid = stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)
    if not valid or info.st_uid != os.getuid() or (not directory and info.st_nlink != 1):
        raise ValueError("unsafe release evidence")
    return info


def digest(path):
    owned(path)
    result = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def installed_digest():
    # Include updater and packaging bytes and executable modes, not just hamn.
    entries = []
    def visit(path, name):
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            owned(path, directory=True)
            entries.append((name, stat.S_IMODE(info.st_mode), None))
            for child in sorted(path.iterdir()):
                visit(child, name + "/" + child.name)
        else:
            entries.append((name, stat.S_IMODE(info.st_mode), digest(path)))
    owned(generation, directory=True)
    visit(generation / "bin", "bin")
    for name in ("scripts", "packaging"):
        visit(generation / "share/hamn/src" / name, name)
    return hashlib.sha256(json.dumps(entries, separators=(",", ":")).encode()).hexdigest()


try:
    identity = {"schemaVersion": 1, "version": version, "hostSHA256": host_hash,
                "guestSHA256": guest_hash}
    if mode == "check":
        info = owned(receipt)
        if stat.S_IMODE(info.st_mode) != 0o600 or info.st_size > 4096:
            raise ValueError("invalid receipt")
        recorded = json.loads(receipt.read_text())
        if not isinstance(recorded, dict) or set(recorded) != set(identity) | {"installedSHA256"}:
            raise ValueError("invalid receipt schema")
        if any(recorded[key] != value for key, value in identity.items()):
            raise ValueError("different release")
        if recorded["installedSHA256"] != installed_digest():
            raise ValueError("installed files changed")
        cache = Path(cache)
        selection = cache / "guest-image.json"
        if owned(selection).st_size > 4096:
            raise ValueError("invalid selection")
        name = "hamn-guest-" + guest_hash + ".img"
        if json.loads(selection.read_text()) != {"schemaVersion": 1, "file": name, "sha256": guest_hash}:
            raise ValueError("different image selection")
        marker = cache / (name + ".verified")
        if owned(marker).st_size > 128 or marker.read_text().strip() != guest_hash:
            raise ValueError("invalid image verification marker")
        if digest(cache / name) != guest_hash:
            raise ValueError("cached image changed")
    elif mode == "write":
        identity["installedSHA256"] = installed_digest()
        fd, temporary = tempfile.mkstemp(prefix=".hamn-release-", dir=generation)
        try:
            with os.fdopen(fd, "w") as output:
                json.dump(identity, output, sort_keys=True, separators=(",", ":"))
                output.write("\n")
                output.flush()
                os.fsync(output.fileno())
            # Never replace a pre-existing file or follow a receipt symlink.
            os.link(temporary, receipt)
        finally:
            os.unlink(temporary)
    else:
        raise ValueError("unknown receipt operation")
except (OSError, ValueError, TypeError, RecursionError) as error:
    if mode == "write":
        print("hamn update: cannot record installed release: " + str(error), file=sys.stderr)
    sys.exit(1)
PY_RECEIPT
}

if [ -n "$old_target" ] && [ "$managed_marker" = version=1 ]; then
    progress "Checking installed release and cached image..."
    if release_receipt check "$old_target" &&
        [ "$(readlink "$hamn_link")" = "$old_target" ] && path_absent "$update_journal"; then
        progress "Unchanged Hamn ${release_version#v}: installed files and guest image match this release."
        progress "No further artifact downloads or installation were needed."
        prune_generations
        exit 0
    fi
fi

previous_version=
if [ -n "$old_target" ] && managed_generation_target "$old_target"; then
    previous_version=$("$old_target" --version) || fail "cannot read installed version"
    previous_version=${previous_version#hamn }
fi
if [ -n "$previous_version" ]; then
    progress "Release: $previous_version -> ${release_version#v}"
else
    progress "Installing Hamn ${release_version#v}"
fi
progress "Existing VMs are not restarted; existing profile disks keep their guest root."
host_archive=$work/host.tar.gz
guest_download=$work/guest.img
progress "Downloading host archive..."
fetch "$host_url" "$host_archive" || fail "host download failed; check your connection and retry"
progress "Downloading guest image (this may take several minutes)..."
fetch "$guest_url" "$guest_download" || fail "guest image download failed; check your connection and retry"
progress "Verifying archive and image SHA-256..."
[ "$(sha256_file "$host_archive")" = "$host_hash" ] ||
    fail "host artifact SHA-256 mismatch"
[ "$(sha256_file "$guest_download")" = "$guest_hash" ] ||
    fail "guest image SHA-256 mismatch"

progress "Extracting verified host archive..."
artifact_root=$(python3 - "$host_archive" "$work/extract" <<'PY'
import os
import posixpath
import sys
import tarfile

archive, destination = sys.argv[1:]
with tarfile.open(archive, "r:gz") as bundle:
    members = bundle.getmembers()
    if not members:
        raise SystemExit("empty host artifact")
    roots = set()
    for member in members:
        name = member.name
        if name.startswith("/") or "\\" in name:
            raise SystemExit("unsafe host artifact path")
        normalized = posixpath.normpath(name)
        if normalized in (".", "..") or normalized.startswith("../") or normalized != name.rstrip("/"):
            raise SystemExit("unsafe host artifact path")
        roots.add(normalized.split("/", 1)[0])
        if not (member.isdir() or member.isreg()):
            raise SystemExit("host artifact contains a non-regular entry")
    if len(roots) != 1:
        raise SystemExit("host artifact must have one top-level directory")
    root = next(iter(roots))
    required = {
        root + "/bin/hamn",
        root + "/scripts/install-host.sh",
        root + "/scripts/update-host.sh",
        root + "/packaging/release/update-manifest-url",
    }
    actual = {member.name.rstrip("/") for member in members}
    if not required.issubset(actual):
        missing = ", ".join(sorted(required - actual))
        raise SystemExit("host artifact is missing required Hamn files: " + missing)
    os.makedirs(destination, mode=0o700, exist_ok=True)
    for member in members:
        bundle.extract(member, destination)
print(root)
PY
) || fail "host artifact validation or extraction failed"
artifact=$work/extract/$artifact_root
[ -x "$artifact/bin/hamn" ] && [ -f "$artifact/scripts/install-host.sh" ] ||
    fail "extracted host artifact is incomplete"

[ "$("$artifact/bin/hamn" --version)" = "hamn ${release_version#v}" ] ||
    fail "host binary version does not match the release manifest"

progress "Staging verified guest image..."
guest_name=hamn-guest-$guest_hash.img
guest_target=$cache/$guest_name
guest_marker=$guest_target.verified
if [ ! -e "$guest_target" ]; then
    guest_stage=$cache/.$guest_name.update.$$
    cp "$guest_download" "$guest_stage"
    chmod 0644 "$guest_stage"
    [ "$(sha256_file "$guest_stage")" = "$guest_hash" ] ||
        fail "staged guest image SHA-256 mismatch"
    mv -f "$guest_stage" "$guest_target"
fi
safe_regular "$guest_target" || fail "staged guest image is unsafe"
[ "$(sha256_file "$guest_target")" = "$guest_hash" ] ||
    fail "cached guest image SHA-256 mismatch"
guest_marker_stage=$(mktemp "$cache/.${guest_name}.verified.XXXXXX") ||
    fail "cannot stage guest image verification marker"
printf '%s\n' "$guest_hash" >"$guest_marker_stage"
chmod 0644 "$guest_marker_stage"
mv -f "$guest_marker_stage" "$guest_marker"

new_selection=$work/guest-image.json
printf '{"schemaVersion":1,"file":"%s","sha256":"%s"}\n' \
    "$guest_name" "$guest_hash" >"$new_selection"
chmod 0600 "$new_selection"

progress "Installing release atomically..."
prepare_update_journal "$new_selection" ||
    fail "cannot record a durable update rollback transaction"
trap 'interrupted_update HUP' HUP
trap 'interrupted_update INT' INT
trap 'interrupted_update TERM' TERM

if ! env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    /bin/bash "$artifact/scripts/install-host.sh" "$artifact/bin/hamn" \
    "$bindir" "$datadir" >"$work/host-install.log" 2>&1; then
    # A closed diagnostic stream must not prevent transaction recovery.
    cat "$work/host-install.log" >&2 || :
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "host install failed and the recovery journal could not be applied"
    fail "host install failed; $recovery_summary"
fi

if ! test_after_host_install_barrier; then
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "update interruption barrier failed and the recovery journal could not be applied"
    fail "update interruption barrier failed; $recovery_summary"
fi

installed_target=$(readlink "$hamn_link")
if ! managed_generation_target "$installed_target" ||
    [ "$(sha256_file "$installed_target")" != "$(sha256_file "$artifact/bin/hamn")" ] ||
    ! release_receipt write "$installed_target"; then
    trap - HUP INT TERM
    rollback_update_journal || fail "release receipt failed and recovery could not be applied"
    fail "release receipt failed; update transaction recovered"
fi
/bin/sync

if ! commit_guest_selection_from_journal; then
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "guest image commit failed and the recovery journal could not be applied"
    fail "guest image commit failed; $recovery_summary"
fi

if ! retire_update_journal completed; then
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "update commit could not clear its recovery journal; retry the same command with all original options (including --manifest) before starting a VM"
    fail "update commit metadata could not be cleared; $recovery_summary"
fi
trap - HUP INT TERM
if ! test_after_journal_retire_barrier; then
    fail "update completion barrier failed; the completed transaction is safely retired"
fi
if ! cleanup_retired_journals; then
    echo "hamn update: completed transaction cleanup is deferred; the committed binary and guest image selection are active" >&2
fi
if ! cleanup_deferred_journals; then
    echo "hamn update: completed transaction cleanup remains deferred; the committed binary and guest image selection are active" >&2
fi

prune_generations

if [ -n "$previous_version" ]; then
    progress "Updated Hamn: $previous_version -> ${release_version#v}"
else
    progress "Installed Hamn ${release_version#v}"
fi
progress "Command: $hamn_link"
progress "Guest image verified and selected for new profile disks. Existing VMs were not restarted."
progress "Verify: $hamn_link --version"
