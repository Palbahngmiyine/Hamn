#!/bin/bash
# Install one HTTPS/digest-verified compatible release without rebuilding it.
set -euo pipefail
export LC_ALL=C
# Stock macOS tools only: a caller PATH with GNU coreutils would change stat -f.
# Regression tests inject tool faults through an explicit directory instead.
PATH=/usr/bin:/bin:/usr/sbin:/sbin
[ -z "${HAMN_TEST_UPDATE_TOOL_DIR:-}" ] || PATH=$HAMN_TEST_UPDATE_TOOL_DIR:$PATH
export PATH

# Failure reporting. With --result-file the frontend prints this single reason
# once (and puts it in the headless JSON error); the installer and direct runs
# print it here. Detailed diagnostics may precede it on stderr.
fail_prefix="hamn upgrade"
result_ready=0
fail() {
    if [ "$result_ready" = 1 ] && printf '%s\n' "$*" >"$result_file" 2>/dev/null; then
        exit 1
    fi
    echo "$fail_prefix: $*" >&2
    exit 1
}

# Human progress goes to stderr as plain sentences; stdout belongs to the
# headless JSON protocol. The native downloader draws its own progress lines.
progress() {
    echo "$*" >&2
}

# Release information could not be used: say why and what to do next.
manifest_failure() {
    case "$1" in
    "download failed: "*)
        fail "could not check for updates: ${1#download failed: }. Check your connection and try again." ;;
    *)
        fail "the latest release information is not usable by this Hamn ($1). Reinstall with the official installer: https://github.com/Palbahngmiyine/Hamn#install" ;;
    esac
}

# The last diagnostic line of a native helper, without its internal prefix.
helper_reason() {
    local line
    line=$(tail -n 1 "$1" 2>/dev/null) || line=
    line=${line#hamn: }
    printf '%s' "${line:-unknown error}"
}

usage() {
    echo "usage: update-host.sh --bindir DIR --datadir DIR [--manifest URL_OR_PATH] [--bootstrap] [--check-only] [--force] [--current-version VERSION] [--output-json]" >&2
    exit 2
}

sha256_file() {
    install_support hash "$1"
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

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
source_root=$(cd "$script_dir/.." && pwd -P)
ROOT=$source_root
source "$script_dir/install-support.sh"
bindir=
datadir=
manifest_ref=
bootstrap=0
check_only=0
force=0
output_json=0
result_file=
current_version=
upgrade_support() { install_support upgrade "$@"; }
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
    --check-only) [ "$check_only" = 0 ] || usage; check_only=1; shift ;;
    --force) [ "$force" = 0 ] || usage; force=1; shift ;;
    --output-json) [ "$output_json" = 0 ] || usage; output_json=1; shift ;;
    --result-file)
        [ "$#" -ge 2 ] && [ -z "$result_file" ] || usage
        result_file=$2; shift 2 ;;
    --current-version)
        [ "$#" -ge 2 ] && [ -z "$current_version" ] || usage
        current_version=$2; shift 2 ;;
    *) usage ;;
    esac
done
[ "$check_only" = 0 ] || [ "$force" = 0 ] || usage
[ "$bootstrap" = 0 ] || fail_prefix="hamn install"
if [ -n "$result_file" ]; then
    safe_private_regular "$result_file" || fail "unsafe upgrade result file"
    result_ready=1
fi
if [ "$check_only" = 1 ]; then
    [ -n "$current_version" ] || usage
    if [ -z "$manifest_ref" ]; then
        safe_regular "$source_root/packaging/release/update-manifest-url" || fail "missing manifest pointer"
        manifest_ref=$(cat "$source_root/packaging/release/update-manifest-url")
    fi
    # This branch precedes directory creation, locks, journal recovery and all
    # installation inspection. A check cannot repair an interrupted mutation.
    check=("$INSTALL_SUPPORT" __install-support upgrade check --manifest "$manifest_ref"
        --current-version "$current_version" --macos "$(sw_vers -productVersion)"
        --architecture "$(uname -m)" --target "$(readlink "$bindir/hamn")")
    [ "$result_ready" = 1 ] || exec "${check[@]}"
    # The JSON result goes to the result file; a failure reason is kept.
    if ! reason=$("${check[@]}" 2>&1 >"$result_file"); then
        reason=${reason##*$'\n'}
        reason=${reason#hamn: }
        manifest_failure "${reason:-unknown error}"
    fi
    exit 0
fi
[ -z "$current_version" ] || upgrade_support version "$current_version" 2>/dev/null || \
    fail "upgrade requires a stable managed release; reinstall with the official installer"
[ -n "$bindir" ] && [ -n "$datadir" ] || usage

BINDIR=$bindir DATADIR=$datadir
source "$script_dir/install-transaction.sh"
bindir=$BINDIR datadir=$DATADIR
prune_generations() {
    # Removed generations are routine; only a deferral (stderr) is reported.
    install_support prune "$bindir" "$datadir" \
        "$old_target" "$source_root" >/dev/null ||
        echo "$fail_prefix: obsolete generation cleanup deferred" >&2
}

hamn_link=$bindir/hamn
bootstrap_entry=$bootstrap
refresh_installation() {
    old_target=
    managed_marker=
    bootstrap=$bootstrap_entry
    # A queued bootstrap may find an installation created by the lock owner.
    # It then has the same validation and rollback obligations as an update.
    if [ "$bootstrap" = 1 ] && ! path_absent "$hamn_link"; then
        [ -L "$hamn_link" ] ||
            fail "$hamn_link is not a managed Hamn generation link (an older standalone Hamn or another program); move it aside and run the installer again"
        bootstrap=0
    fi
    [ "$bootstrap" = 0 ] || return 0
    safe_directory "$bindir" || fail "unsafe managed binary directory: $bindir"
    safe_directory "$datadir" || fail "unsafe managed data directory: $datadir"
    safe_regular "$datadir/.hamn-managed" ||
        fail "managed data marker is missing or unsafe"
    managed_marker=$(cat "$datadir/.hamn-managed")
    [ -n "$managed_marker" ] ||
        fail "$datadir is a pre-release Hamn install (empty data marker), which is no longer migrated; move it aside and run the installer again"
    [ "$managed_marker" = version=1 ] || fail "managed data marker is invalid"
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
}
refresh_installation

[ -n "${HOME:-}" ] || fail "HOME is not set"
runtime_root=$HOME/.hamn
if [ ! -d "$runtime_root" ]; then
    mkdir -m 0700 "$runtime_root"
fi
safe_private_directory "$runtime_root" ||
    fail "unsafe Hamn runtime root"
cache=$runtime_root/cache
if [ ! -d "$cache" ]; then
    mkdir -m 0755 "$cache"
fi
safe_directory "$cache" || fail "unsafe Hamn image cache"

# Hold the cache recovery lock in addition to the two install-root locks.
# Descriptors 6/7 belong to install-transaction.sh and 8/9 to install-host.
# A distinct inherited descriptor keeps nested installation and recovery atomic.
transaction_lock=$cache/.hamn-upgrade.lock
install_support lock-prepare "$transaction_lock"
exec 5>>"$transaction_lock"
install_support lock-acquire "$transaction_lock" 5

guest_selection=$cache/guest-image.json
update_journal=$cache/.hamn-update-transaction
journal_directory=$update_journal
journal_bootstrap=
journal_selection_state=
journal_old_target=
journal_attempt=
journal_stage=
host_mutation=1
journal_host_mutation=1
journal_new_target=
journal_legacy=
journal_legacy_path=

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

# A journal records: state (version=3, bootstrap, selection, hostMutation),
# attempt, new-selection, new-target (empty until install-host publishes the
# attempted generation), old-target (updates only) and previous-selection
# (when a selection existed). Journals of other versions are refused, and
# journal_legacy names a v1/v2 journal from Hamn 0.1.2 or earlier, which has
# no attempted-target identity and so cannot be recovered automatically.
journal_state_pattern=$'^version=3\nbootstrap=([01])\nselection=(present|absent)\nhostMutation=([01])$'
load_update_journal() {
    local state attempt expected_count
    journal_legacy=
    safe_private_directory "$journal_directory" || return 1
    safe_private_regular "$journal_directory/state" || return 1
    state=$(cat "$journal_directory/state") || return 1
    case "$state" in
    version=1$'\n'*|version=2$'\n'*)
        journal_legacy=${state%%$'\n'*}
        journal_legacy=${journal_legacy#version=}
        journal_legacy_path=$journal_directory
        return 1
        ;;
    esac
    [[ "$state" =~ $journal_state_pattern ]] || return 1
    journal_bootstrap=${BASH_REMATCH[1]}
    journal_selection_state=${BASH_REMATCH[2]}
    journal_host_mutation=${BASH_REMATCH[3]}
    safe_private_regular "$journal_directory/attempt" || return 1
    safe_private_regular "$journal_directory/new-selection" || return 1
    safe_private_regular "$journal_directory/new-target" || return 1
    attempt=$(cat "$journal_directory/attempt") || return 1
    [[ "$attempt" =~ ^[A-Za-z0-9]{6}$ ]] || return 1
    journal_new_target=$(cat "$journal_directory/new-target") || return 1
    if [ -n "$journal_new_target" ]; then
        managed_generation_target "$journal_new_target" || return 1
        [ "$(stat -f '%z' "$journal_directory/new-target")" = "$((${#journal_new_target} + 1))" ] || return 1
    else
        [ ! -s "$journal_directory/new-target" ] || return 1
    fi
    # A bootstrap always installs a host; a selection-only repair never does.
    [ "$journal_host_mutation" = 1 ] || [ "$journal_bootstrap" = 0 ] || return 1
    [ "$journal_host_mutation" = 1 ] || [ -z "$journal_new_target" ] || return 1
    expected_count=4
    [ "$journal_bootstrap" = 1 ] || expected_count=$((expected_count + 1))
    [ "$journal_selection_state" = absent ] || expected_count=$((expected_count + 1))
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
        "$stage/previous-selection" "$stage/old-target" "$stage/new-target"
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
        echo "$fail_prefix: transaction retirement is pending a filesystem flush" >&2
}

cleanup_deferred_journal() {
    local deferred=$1 entry name
    safe_private_directory "$deferred" || return 1
    while IFS= read -r -d '' entry; do
        name=${entry##*/}
        case "$name" in
        state|attempt|new-selection|previous-selection|old-target|new-target)
            safe_private_regular "$entry" || return 1
            ;;
        *) return 1 ;;
        esac
    done < <(find "$deferred" -mindepth 1 -maxdepth 1 -print0)
    rm -f "$deferred/state" "$deferred/attempt" "$deferred/new-selection" \
        "$deferred/previous-selection" "$deferred/old-target" "$deferred/new-target" || return 1
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
        echo "$fail_prefix: retired transaction cleanup is pending a filesystem flush" >&2
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
    # Root locks serialize current writers but do not make another HOME's older
    # journal authoritative. Check identity before changing even our selection.
    if ! journal_owns_active_generation; then
        echo "$fail_prefix: pending transaction does not own the active generation; preserving its journal and both selections" >&2
        echo "$fail_prefix: the active generation is neither the journal's previous nor its attempted generation; automatic rollback cannot prove ownership" >&2
        echo "$fail_prefix: manual review of the retained journal and generation history is required; retrying alone will not resolve this ambiguity" >&2
        return 1
    fi
    restore_guest_selection_from_journal || return 1
    if [ "$journal_host_mutation" = 1 ]; then
        restore_binary_link_from_journal || return 1
    fi
    recovery_summary="prior binary and guest image selection were restored"
    if [ "$journal_bootstrap" = 1 ]; then
        recovery_summary="guest image selection was restored; no previous binary was recorded, so a published command may remain"
    fi
    retire_update_journal recovered
}

journal_owns_active_generation() {
    local current_target
    if path_absent "$hamn_link"; then
        # A first install may have recorded its target but not exposed it yet.
        [ "$journal_bootstrap" = 1 ]
        return
    fi
    [ -L "$hamn_link" ] || return 1
    current_target=$(readlink "$hamn_link") || return 1
    managed_generation_target "$current_target" || return 1
    if [ "$journal_bootstrap" = 0 ] && [ "$current_target" = "$journal_old_target" ]; then
        return 0
    fi
    # Only the recorded attempted target identifies this transaction's own
    # install; an unrelated later install (another HOME) must be preserved.
    [ "$journal_host_mutation" = 1 ] &&
        [ -n "$journal_new_target" ] && [ "$current_target" = "$journal_new_target" ]
}

# A v1/v2 journal (see load_update_journal) is refused, never guessed at.
# $1 is pending (an interrupted transaction) or finished (a retired one).
legacy_journal_failure() {
    if [ "$1" = pending ]; then
        fail "an interrupted update from Hamn 0.1.2 or earlier left a v$journal_legacy journal at $journal_legacy_path, which this Hamn cannot recover. Check that $hamn_link and the guest image selection are as you want them (the journal's old-target and previous-selection hold the prior values), then move the journal aside and run the command again"
    fi
    fail "a finished v$journal_legacy update journal from Hamn 0.1.2 or earlier remains at $journal_legacy_path; move it aside and run the command again"
}

recover_pending_update() {
    path_absent "$update_journal" && return 0
    if ! rollback_update_journal; then
        echo "$fail_prefix: incomplete prior update could not be safely recovered" >&2
        return 1
    fi
    if [ "$journal_bootstrap" = 0 ]; then
        echo "$fail_prefix: recovered the previous binary and guest image selection after an interrupted update" >&2
    else
        progress "Recovered interrupted bootstrap: $recovery_summary"
    fi
}

prepare_update_journal() {
    local new_selection=$1 selection_state expected_attempt
    journal_directory=$update_journal
    path_absent "$update_journal" || {
        echo "$fail_prefix: another update transaction is already active" >&2
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
        if ! install_support recovery-root "$old_target" "$cache"
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
        printf 'version=3\n'
        printf 'bootstrap=%s\n' "$bootstrap"
        printf 'selection=%s\n' "$selection_state"
        printf 'hostMutation=%s\n' "$host_mutation"
    } >"$journal_stage/state"
    # install-host fills this slot durably before it publishes the new link.
    # Empty means this transaction has not published a host generation.
    : >"$journal_stage/new-target"
    printf '%s\n' "$journal_attempt" >"$journal_stage/attempt"
    chmod 0600 "$journal_stage/state" "$journal_stage/attempt" "$journal_stage/new-target" || {
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

test_transaction_barrier() {
    local name=$1 ready_name release_name ready release
    ready_name=HAMN_TEST_UPDATE_${name}_READY_FIFO
    release_name=HAMN_TEST_UPDATE_${name}_RELEASE_FIFO
    ready=${!ready_name:-}
    release=${!release_name:-}
    [ -z "$ready" ] && [ -z "$release" ] && return 0
    [ -n "$ready" ] && [ -n "$release" ] && [ -p "$ready" ] && [ -p "$release" ] || return 1
    printf 'ready\n' >"$ready"
    IFS= read -r _ <"$release"
}

interrupted_update() {
    local signal=${1:-TERM} status
    trap - HUP INT TERM
    if ! rollback_update_journal; then
        echo "$fail_prefix: interrupted by $signal; recovery journal remains for a later safe recovery" >&2
    else
        echo "$fail_prefix: interrupted by $signal; $recovery_summary" >&2
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
if ! cleanup_retired_journals; then
    [ -z "$journal_legacy" ] || legacy_journal_failure finished
    fail "a retired update transaction is unsafe or could not be cleaned"
fi
if ! recover_pending_update; then
    [ -z "$journal_legacy" ] || legacy_journal_failure pending
    fail "previous update recovery failed; no new update was installed"
fi
cleanup_retired_journals ||
    fail "the recovered update transaction could not be cleaned"
cleanup_deferred_journals ||
    fail "the recovered transaction cleanup is unsafe or could not be cleaned"

# Both recovery and an earlier lock owner can change the installation. Bind
# rollback, downgrade checks and bootstrap mode to the state under this lock.
refresh_installation
if [ "$bootstrap" = 0 ]; then
    managed_generation_target "$old_target" || fail "recovered hamn link is unsafe"
    if [ -n "$current_version" ] && [ "$bootstrap_entry" = 0 ] && \
        [ "$source_root" != "${old_target%/bin/hamn}/share/hamn/src" ]; then
        fail "managed generation changed while waiting or recovering; rerun the managed hamn command"
    fi
    current_version=$("$old_target" --version) || fail "cannot read installed version"
    current_version=${current_version#hamn }
else
    current_version=0.0.0
fi
upgrade_support version "$current_version" >/dev/null 2>&1 ||
    fail "active installation is not a stable managed release; reinstall with the official installer"

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
counts=$work/counts
mkdir -m 0700 "$counts"
# The installer supplies a verified local manifest; only updates contact it.
[ "$bootstrap_entry" = 1 ] || progress "Checking for updates..."
if ! manifest_bytes=$(upgrade_support manifest --manifest "$manifest_ref" \
    --current-version "$current_version" --macos "$(sw_vers -productVersion)" \
    --architecture "$(uname -m)" --output "$manifest" 2>"$work/manifest.err"); then
    manifest_failure "$(helper_reason "$work/manifest.err")"
fi
printf '{"downloadedBytes":%s,"resumedBytes":0,"reusedBytes":0,"source":"manifest"}\n' \
    "$manifest_bytes" >"$counts/manifest.json"
chmod 0600 "$counts/manifest.json"
upgrade_support fields "$manifest" >"$work/manifest-fields"
{
    IFS= read -r release_version
    IFS= read -r host_url
    IFS= read -r host_hash
    IFS= read -r guest_url
    IFS= read -r guest_hash
} <"$work/manifest-fields"
status=$(upgrade_support status "$manifest" "$current_version" "$cache" "$old_target") || fail "unsupported installed version"
if [ "$status" = ahead ]; then
    [ "$bootstrap_entry" = 0 ] ||
        fail "stable downgrade is not permitted: Hamn $current_version is already installed, which is newer than this installer (${release_version#v}); run hamn upgrade to stay current"
    fail "stable downgrade is not permitted: installed Hamn $current_version is newer than the latest release (${release_version#v})"
fi
finish_result() {
    if [ "$output_json" = 1 ]; then
        if [ -n "$result_file" ]; then
            safe_private_regular "$result_file" || fail "unsafe upgrade result file"
            upgrade_support result "$manifest" "$current_version" "$1" "$counts" >"$result_file"
        else
            upgrade_support result "$manifest" "$current_version" "$1" "$counts"
        fi
    fi
}

# A receipt is advisory: absent, malformed, unsafe, or stale evidence forces the
# normal verified install. It binds release identities to the installed files;
# a version string alone never permits skipping downloads. Each generation owns
# its private receipt, so rollback restores the previous receipt with the link.
release_receipt() {
    if [ "$1" = write ]; then
        upgrade_support receipt "$manifest" "$1" "$2" "$cache"
    else
        upgrade_support receipt "$manifest" "$1" "$2" "$cache" 2>/dev/null
    fi
}

if [ -n "$old_target" ]; then
    if [ "$force" = 0 ] && [ "$status" = up-to-date ] && release_receipt check "$old_target" &&
        [ "$(readlink "$hamn_link")" = "$old_target" ] && path_absent "$update_journal"; then
        # Installed files and guest image match this release: no downloads.
        if [ "$bootstrap_entry" = 1 ]; then
            progress "Hamn ${release_version#v} is already installed."
        else
            progress "Hamn ${release_version#v} is up to date."
        fi
        upgrade_support reuse-counts "$manifest" "$counts" both
        finish_result up-to-date
        prune_generations
        exit 0
    fi
fi

previous_version=
[ -z "$old_target" ] || previous_version=$current_version
if [ "$force" = 0 ] && [ "$status" = repair-required ] && \
    [ -n "$old_target" ] && release_receipt host-check "$old_target"; then
    host_mutation=0
    upgrade_support reuse-counts "$manifest" "$counts" host
fi
if [ -n "$previous_version" ] && [ "$host_mutation" = 0 ]; then
    progress "Repairing the Hamn ${release_version#v} guest image..."
elif [ -n "$previous_version" ] && [ "$previous_version" = "${release_version#v}" ]; then
    progress "Reinstalling Hamn ${release_version#v}..."
elif [ -n "$previous_version" ]; then
    progress "Updating Hamn $previous_version → ${release_version#v}..."
fi
# The native helper prints download progress itself (nothing for a verified
# cache hit) and the transfer reason on failure; stdout is only the path.
acquisition_failed() {
    local what=$1 reason=
    [ ! -f "$counts/$2.reason" ] || reason=$(head -c 1024 "$counts/$2.reason")
    case "$reason" in
    "download failed: "*)
        if [ "$bootstrap_entry" = 1 ]; then
            fail "could not download $what: ${reason#download failed: }. Check your connection and run the installer again to resume."
        fi
        fail "could not download $what: ${reason#download failed: }. Check your connection and run hamn upgrade again to resume."
        ;;
    *) fail "could not obtain $what: ${reason:-unknown error}" ;;
    esac
}
if [ "$host_mutation" = 1 ]; then
    host_archive=$(upgrade_support acquire "$manifest" host "$cache" "$counts/host.json") ||
        acquisition_failed "Hamn ${release_version#v}" host
fi
guest_download=$(upgrade_support acquire "$manifest" guestImage "$cache" "$counts/guestImage.json") ||
    acquisition_failed "the guest image" guestImage
progress "Installing..."
[ "$(sha256_file "$guest_download")" = "$guest_hash" ] || fail "guest image SHA-256 mismatch"
if [ "$host_mutation" = 1 ]; then
[ "$(sha256_file "$host_archive")" = "$host_hash" ] || fail "host artifact SHA-256 mismatch"
artifact_root=$(install_support extract "$host_archive" "$work/extract") ||
    fail "host artifact validation or extraction failed"

artifact=$work/extract/$artifact_root
[ -x "$artifact/bin/hamn" ] && [ -f "$artifact/scripts/install-host.sh" ] ||
    fail "extracted host artifact is incomplete"

[ "$("$artifact/bin/hamn" --version)" = "hamn ${release_version#v}" ] ||
    fail "host binary version does not match the release manifest"
fi

guest_name=hamn-guest-$guest_hash.img
guest_target=$cache/$guest_name
guest_marker=$guest_target.verified
if [ -e "$guest_target" ] || [ -L "$guest_target" ]; then
    safe_regular "$guest_target" || fail "cached guest image is unsafe"
    if [ "$(sha256_file "$guest_target")" != "$guest_hash" ]; then
        rm "$guest_target" || fail "cannot remove damaged owned guest image"
    fi
fi
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
if ! test_transaction_barrier PREPARED; then
    rollback_update_journal || fail "prepared transaction barrier recovery failed"
    fail "prepared transaction barrier failed"
fi

if [ "$host_mutation" = 1 ]; then
if ! env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    /bin/bash "$artifact/scripts/install-host.sh" "$artifact/bin/hamn" \
    "$bindir" "$datadir" "$update_journal" >"$work/host-install.log" 2>&1; then
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
if ! load_update_journal || [ "$journal_new_target" != "$installed_target" ] ||
    ! managed_generation_target "$installed_target" ||
    [ "$(sha256_file "$installed_target")" != "$(sha256_file "$artifact/bin/hamn")" ] ||
    ! release_receipt write "$installed_target"; then
    trap - HUP INT TERM
    rollback_update_journal || fail "release receipt failed and recovery could not be applied"
    fail "release receipt failed; update transaction recovered"
fi
/bin/sync
fi

if ! commit_guest_selection_from_journal; then
    trap - HUP INT TERM
    rollback_update_journal ||
        fail "guest image commit failed and the recovery journal could not be applied"
    fail "guest image commit failed; $recovery_summary"
fi
if ! test_transaction_barrier AFTER_GUEST_SELECTION; then
    rollback_update_journal || fail "guest selection barrier recovery failed"
    fail "guest selection barrier failed"
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
    echo "$fail_prefix: completed transaction cleanup is deferred; the committed binary and guest image selection are active" >&2
fi
if ! cleanup_deferred_journals; then
    echo "$fail_prefix: completed transaction cleanup remains deferred; the committed binary and guest image selection are active" >&2
fi

prune_generations

if [ -z "$previous_version" ]; then
    progress "Installed Hamn ${release_version#v}."
elif [ "$host_mutation" = 0 ]; then
    progress "Repaired the Hamn ${release_version#v} guest image. Existing VMs were not restarted."
elif [ "$previous_version" = "${release_version#v}" ]; then
    progress "Reinstalled Hamn ${release_version#v}. Existing VMs were not restarted."
else
    progress "Updated Hamn $previous_version → ${release_version#v}. Existing VMs were not restarted."
fi

# The notice is advisory and never authorizes installation. Drop it only after
# a successful transaction; an unsafe entry is left for explicit repair.
if safe_private_regular "$cache/update-notice-v1.json"; then
    rm "$cache/update-notice-v1.json" || true
fi
if [ "$host_mutation" = 0 ]; then finish_result repaired; else finish_result updated; fi
