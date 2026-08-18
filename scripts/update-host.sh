#!/bin/bash
# Install one signed, compatible Hamn release without rebuilding it locally.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn update: $*" >&2
    exit 1
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
    case "$source" in
    https://*)
        curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 \
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

hamn_link=$bindir/hamn
old_target=
if [ "$bootstrap" = 0 ]; then
    safe_directory "$bindir" || fail "unsafe managed binary directory: $bindir"
    safe_directory "$datadir" || fail "unsafe managed data directory: $datadir"
    safe_regular "$datadir/.hamn-managed" ||
        fail "managed data marker is missing or unsafe"
    [ "$(cat "$datadir/.hamn-managed")" = 'version=1' ] ||
        fail "managed data marker is invalid"
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
    local target=$1 relative
    case "$target" in
    "$datadir/.hamn-generations/"*) ;;
    *) return 1 ;;
    esac
    relative=${target#"$datadir/.hamn-generations/"}
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
    retire_update_journal recovered
}

recover_pending_update() {
    path_absent "$update_journal" && return 0
    if ! rollback_update_journal; then
        echo "hamn update: incomplete prior update could not be safely recovered" >&2
        return 1
    fi
    echo "hamn update: recovered the previous binary and guest image selection after an interrupted update" >&2
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
        echo "hamn update: interrupted by $signal; previous binary and guest image selection were restored" >&2
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
fetch "$manifest_ref" "$manifest"

python3 - "$manifest" "$(sw_vers -productVersion)" "$(uname -m)" <<'PY' \
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
    if manifest["validationMode"] != "github-hosted-no-vm":
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

print(host_url)
print(host_hash)
print(guest_url)
print(guest_hash)
PY
{
    IFS= read -r host_url
    IFS= read -r host_hash
    IFS= read -r guest_url
    IFS= read -r guest_hash
} <"$work/manifest-fields"
[ -n "$host_url" ] && [ -n "$host_hash" ] && [ -n "$guest_url" ] &&
    [ -n "$guest_hash" ] || fail "immutable manifest fields are incomplete"

host_archive=$work/host.tar.gz
guest_download=$work/guest.img
fetch "$host_url" "$host_archive"
fetch "$guest_url" "$guest_download"
[ "$(sha256_file "$host_archive")" = "$host_hash" ] ||
    fail "host artifact SHA-256 mismatch"
[ "$(sha256_file "$guest_download")" = "$guest_hash" ] ||
    fail "guest image SHA-256 mismatch"

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

prepare_update_journal "$new_selection" ||
    fail "cannot record a durable update rollback transaction"
trap 'interrupted_update HUP' HUP
trap 'interrupted_update INT' INT
trap 'interrupted_update TERM' TERM

if ! env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    /bin/bash "$artifact/scripts/install-host.sh" "$artifact/bin/hamn" \
    "$bindir" "$datadir"; then
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "host install failed and the recovery journal could not be applied"
    fail "host install failed; prior binary and guest image selection were restored"
fi

if ! test_after_host_install_barrier; then
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "update interruption barrier failed and the recovery journal could not be applied"
    fail "update interruption barrier failed; prior binary and guest image selection were restored"
fi

if ! commit_guest_selection_from_journal; then
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "guest image commit failed and the recovery journal could not be applied"
    fail "guest image commit failed; prior binary and guest image selection were restored"
fi

if ! retire_update_journal completed; then
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "update commit could not clear its recovery journal; run hamn update again before starting a VM"
    fail "update commit metadata could not be cleared; prior binary and guest image selection were restored"
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

echo "updated Hamn from immutable release manifest: $manifest_ref"
