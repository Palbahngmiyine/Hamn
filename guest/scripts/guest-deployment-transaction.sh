#!/bin/bash
set -u -o pipefail

umask 077

RECOVERY_SCRIPT=/usr/local/libexec/hamn/guest-deployment-transaction

fail() {
    echo "hamn: guest deployment transaction: $*" >&2
    exit 1
}

[ "$#" -eq 2 ] || fail "usage: begin|commit|rollback TOKEN"
ACTION=$1
TOKEN=$2
case "$ACTION" in
    begin|commit|rollback) ;;
    *) fail "unsupported action" ;;
esac
[[ "$TOKEN" =~ ^[0-9a-f]{32}$ ]] || fail "invalid operation token"

ROOT=${HAMN_DEPLOYMENT_TEST_ROOT:-}
if [ -n "$ROOT" ]; then
    [ "$EUID" -ne 0 ] || fail "test root is forbidden for root execution"
    case "$ROOT" in
        /*) ;;
        *) fail "test root must be absolute" ;;
    esac
    case "$ROOT" in
        */|*//*|*/./*|*/../*|*/.|*/..)
            fail "test root must be normalized"
            ;;
    esac
    [ "$ROOT" != / ] || fail "test root cannot be filesystem root"
    [ -d "$ROOT" ] || fail "test root must already exist"
    [ ! -L "$ROOT" ] || fail "test root cannot be a symlink"
    CANONICAL_ROOT=$(cd -- "$ROOT" 2>/dev/null && pwd -P) ||
        fail "cannot resolve test root"
    [ "$CANONICAL_ROOT" = "$ROOT" ] ||
        fail "test root cannot contain symlink parents"
else
    [ "$EUID" -eq 0 ] || fail "root privileges are required"
fi

rooted() {
    local logical=$1
    printf '%s%s' "$ROOT" "$logical"
}

assert_parent_chain_safe() {
    local logical=$1
    local parent=${logical%/*}
    local current=
    local old_ifs=$IFS
    local component physical
    local -a components
    IFS=/
    read -r -a components <<<"$parent"
    IFS=$old_ifs
    for component in "${components[@]}"; do
        [ -n "$component" ] || continue
        current="$current/$component"
        physical=$(rooted "$current")
        [ ! -L "$physical" ] ||
            fail "refusing symlink parent: $current"
    done
}

TRANSACTION_ROOT_LOGICAL=/var/lib/hamn/deployment-transactions
assert_parent_chain_safe "$TRANSACTION_ROOT_LOGICAL/transaction"
TRANSACTION_ROOT=$(rooted "$TRANSACTION_ROOT_LOGICAL")
mkdir -p -- "$TRANSACTION_ROOT" || fail "cannot create transaction root"
[ -d "$TRANSACTION_ROOT" ] && [ ! -L "$TRANSACTION_ROOT" ] ||
    fail "transaction root is unsafe"
[ -O "$TRANSACTION_ROOT" ] || fail "transaction root has the wrong owner"
chmod 0700 "$TRANSACTION_ROOT" || fail "cannot secure transaction root"
TRANSACTION="$TRANSACTION_ROOT/$TOKEN"

ITEM_KEYS=(
    hamnd libexec_hamn hamnd_unit etc_hamn containerd_config docker_config
    docker_dropin host_dns_config host_dns_unit modules_config sysctl_config cni_bin
)
ITEM_PATHS=(
    /usr/local/bin/hamnd
    /usr/local/libexec/hamn
    /etc/systemd/system/hamnd.service
    /etc/hamn
    /etc/containerd/config.toml
    /etc/docker/daemon.json
    /etc/systemd/system/docker.service.d
    /etc/dnsmasq.d/hamn-host-dns.conf
    /etc/systemd/system/hamn-host-dns.service
    /etc/modules-load.d/hamn-kubernetes.conf
    /etc/sysctl.d/99-hamn-kubernetes.conf
    /opt/cni/bin
)
ITEM_TYPES=(
    file directory file directory file file directory file file file file directory
)
SERVICES=(hamnd.service containerd.service docker.service hamn-host-dns.service)

validate_item_type() {
    local path=$1
    local type=$2
    [ ! -L "$path" ] || return 1
    case "$type" in
        file) [ -f "$path" ] ;;
        directory) [ -d "$path" ] ;;
        *) return 1 ;;
    esac
}

# A published phase is exactly the bytes "ready\n". A missing, empty or
# zero-filled phase (a crash can leave any of them) is not ready, and its
# backup is never restored. Bytes are compared because command substitution
# would drop NULs. Returns 0 when ready, 1 when not, and 2 when the phase
# cannot be read: its backup may be ready, so it is neither restored nor
# treated as not ready.
phase_is_ready() {
    local phase=$1 bytes
    [ -f "$phase" ] && [ ! -L "$phase" ] || return 1
    bytes=$(head -c 7 -- "$phase" | od -An -tx1 | tr -d ' \n') || return 2
    [ "$bytes" = 72656164790a ]
}

refuse_pending_transaction() {
    local entry name ready=0
    local -a entries
    shopt -s nullglob dotglob
    entries=("$TRANSACTION_ROOT"/*)
    shopt -u nullglob dotglob
    [ "${#entries[@]}" -eq 0 ] || {
        entry=${entries[0]}
        name=${entry##*/}
        if [[ ! "$name" =~ ^[0-9a-f]{32}$ ]] ||
           [ ! -d "$entry" ] || [ -L "$entry" ] || [ ! -O "$entry" ]; then
            fail "transaction root contains an unsafe entry; inspect $TRANSACTION_ROOT"
        fi
        phase_is_ready "$entry/phase" || ready=$?
        [ "$ready" -ne 2 ] ||
            fail "cannot read the phase of deployment transaction $name; inspect $entry"
        if [ "$ready" -eq 0 ]; then
            fail "pending deployment transaction $name exists at $entry; "\
"recover it before retrying: sudo bash $RECOVERY_SCRIPT "\
"rollback $name"
        fi
        if [ -L "$entry/phase" ] ||
           { [ -e "$entry/phase" ] && [ ! -f "$entry/phase" ]; }; then
            fail "deployment transaction $name has an unsafe phase; inspect $entry"
        fi
        fail "deployment transaction $name at $entry has no ready backup; "\
"it is never restored, and deployment recovery discards it"
    }
}

begin_transaction() {
    local cleanup_incomplete index key logical type source metadata
    local service enabled active
    refuse_pending_transaction
    mkdir -m 0700 -- "$TRANSACTION" || fail "cannot create transaction"
    cleanup_incomplete=1
    cleanup_begin() {
        local rc=$?
        if [ "$cleanup_incomplete" -eq 1 ]; then
            # A phase published before a failed barrier is retired as
            # retire_transaction does: removed and made durable before the
            # rest, so a crash during the removal leaves an entry that
            # recovery discards, never a ready phase with part of the backup.
            # If the retirement cannot be made durable, the backup stays
            # whole; everything in it was durable before ready was published.
            if [ -e "$TRANSACTION/phase" ] || [ -L "$TRANSACTION/phase" ]; then
                rm -f -- "$TRANSACTION/phase" && sync -- "$TRANSACTION" ||
                    exit "$rc"
            fi
            rm -rf -- "$TRANSACTION" || true
        fi
        exit "$rc"
    }
    trap cleanup_begin EXIT
    mkdir -m 0700 -- "$TRANSACTION/data" "$TRANSACTION/meta" ||
        fail "cannot create backup directories"

    for index in "${!ITEM_KEYS[@]}"; do
        key=${ITEM_KEYS[$index]}
        logical=${ITEM_PATHS[$index]}
        type=${ITEM_TYPES[$index]}
        assert_parent_chain_safe "$logical"
        source=$(rooted "$logical")
        metadata="$TRANSACTION/meta/$key"
        if [ -e "$source" ] || [ -L "$source" ]; then
            validate_item_type "$source" "$type" ||
                fail "refusing unsafe $logical"
            cp -a -- "$source" "$TRANSACTION/data/$key" ||
                fail "cannot back up $logical"
            printf 'present\n' >"$metadata" ||
                fail "cannot record $logical"
        else
            printf 'absent\n' >"$metadata" ||
                fail "cannot record absent $logical"
        fi
        chmod 0600 "$metadata" || fail "cannot secure item metadata"
    done

    command -v systemctl >/dev/null 2>&1 || fail "systemctl is unavailable"
    for service in "${SERVICES[@]}"; do
        enabled=$(systemctl is-enabled "$service" 2>/dev/null || true)
        enabled=${enabled%%$'\n'*}
        case "$enabled" in
            enabled|enabled-runtime|disabled|masked|masked-runtime|static|\
            indirect|generated|transient|not-found) ;;
            *) fail "unexpected enablement state for $service" ;;
        esac
        if systemctl is-active --quiet "$service"; then
            active=active
        else
            active=inactive
        fi
        printf '%s\n' "$enabled" \
            >"$TRANSACTION/meta/$service.enabled" ||
            fail "cannot record enablement for $service"
        printf '%s\n' "$active" \
            >"$TRANSACTION/meta/$service.active" ||
            fail "cannot record activity for $service"
        chmod 0600 "$TRANSACTION/meta/$service.enabled" \
            "$TRANSACTION/meta/$service.active" ||
            fail "cannot secure service metadata"
    done
    # Publish ready only after every regular backup file and directory is
    # durable. find(1) does not follow CNI symlinks into the live runtime, and
    # sync(1) fsyncs each operand.
    find "$TRANSACTION" \( -type f -o -type d \) -print0 | xargs -0 sync -- ||
        fail "cannot make the deployment backup durable"
    (set -C; printf 'ready\n' >"$TRANSACTION/.phase") &&
        sync -- "$TRANSACTION/.phase" &&
        mv -f -- "$TRANSACTION/.phase" "$TRANSACTION/phase" &&
        sync -- "$TRANSACTION" "$TRANSACTION_ROOT" ||
        fail "cannot publish the deployment backup"
    cleanup_incomplete=0
    trap - EXIT
}

load_transaction() {
    local ready=0
    [ -d "$TRANSACTION" ] && [ ! -L "$TRANSACTION" ] ||
        fail "transaction does not exist"
    [ -O "$TRANSACTION" ] || fail "transaction has the wrong owner"
    [ -d "$TRANSACTION/data" ] && [ ! -L "$TRANSACTION/data" ] ||
        fail "transaction data is unsafe"
    [ -d "$TRANSACTION/meta" ] && [ ! -L "$TRANSACTION/meta" ] ||
        fail "transaction metadata is unsafe"
    [ ! -L "$TRANSACTION/phase" ] &&
        { [ ! -e "$TRANSACTION/phase" ] || [ -f "$TRANSACTION/phase" ]; } ||
        fail "transaction phase is unsafe"
    phase_is_ready "$TRANSACTION/phase" || ready=$?
    [ "$ready" -ne 2 ] || fail "cannot read the transaction phase"
    [ "$ready" -eq 0 ] || fail "transaction is incomplete"
}

# Make each protected item, and the directory that holds it, durable. find(1)
# does not follow CNI symlinks into the live runtime; a symlink is durable
# with its directory.
sync_protected_items() {
    local logical path
    for logical in "${ITEM_PATHS[@]}"; do
        path=$(rooted "$logical")
        if [ -d "${path%/*}" ]; then
            printf '%s\0' "${path%/*}"
        fi
        if [ -e "$path" ] && [ ! -L "$path" ]; then
            find "$path" \( -type f -o -type d \) -print0
        fi
    done | xargs -0 sync --
}

# Retire a ready backup once commit or rollback has decided its outcome.
# Removing phase is the decision point: from then on the backup is never
# restored. It follows a durable runtime and precedes every other removal, so
# a crash that loses part of the removal leaves an entry that recovery
# discards, never a ready backup with missing or zero-filled data. The
# runtime also includes what the backup does not hold, such as systemd's
# enablement links under /etc/systemd/system and the binfmt databases under
# /var/lib, so the root filesystem as a whole (syncfs) follows the items.
retire_transaction() {
    local runtime=$1
    sync_protected_items && sync -f -- "$(rooted /)" ||
        fail "cannot make the $runtime runtime durable; backup retained at $TRANSACTION"
    rm -f -- "$TRANSACTION/phase" ||
        fail "cannot retire the backup; it is retained at $TRANSACTION"
    sync -- "$TRANSACTION" ||
        fail "retired the backup at $TRANSACTION but cannot make that durable"
    rm -rf -- "$TRANSACTION" ||
        fail "retired the backup but cannot remove $TRANSACTION; "\
"deployment recovery discards it"
    sync -- "$TRANSACTION_ROOT" ||
        fail "removed the retired backup but cannot make the removal durable"
}

ROLLBACK_FAILED=0
rollback_error() {
    echo "hamn: guest deployment transaction: $*" >&2
    ROLLBACK_FAILED=1
}

restore_item() {
    local key=$1
    local logical=$2
    local type=$3
    local metadata="$TRANSACTION/meta/$key"
    local state destination backup parent
    if [ ! -f "$metadata" ] || [ -L "$metadata" ]; then
        rollback_error "missing metadata for $logical"
        return
    fi
    state=$(cat "$metadata")
    case "$state" in
        present|absent) ;;
        *) rollback_error "invalid metadata for $logical"; return ;;
    esac
    assert_parent_chain_safe "$logical"
    destination=$(rooted "$logical")
    if [ "$state" = present ]; then
        backup="$TRANSACTION/data/$key"
        if ! validate_item_type "$backup" "$type"; then
            rollback_error "unsafe backup for $logical"
            return
        fi
    fi
    if ! rm -rf -- "$destination"; then
        rollback_error "cannot clear $logical"
        return
    fi
    [ "$state" = present ] || return
    parent=${destination%/*}
    if ! mkdir -p -- "$parent" || ! cp -a -- "$backup" "$destination"; then
        rollback_error "cannot restore $logical"
    fi
}

read_service_state() {
    local service=$1
    local kind=$2
    local metadata="$TRANSACTION/meta/$service.$kind"
    [ -f "$metadata" ] && [ ! -L "$metadata" ] || return 1
    cat "$metadata"
}

restore_service() {
    local service=$1
    local enabled active remask=
    enabled=$(read_service_state "$service" enabled) || {
        rollback_error "missing enablement state for $service"
        return
    }
    active=$(read_service_state "$service" active) || {
        rollback_error "missing activity state for $service"
        return
    }
    case "$enabled" in
        enabled)
            systemctl unmask "$service" >/dev/null 2>&1 ||
                rollback_error "cannot unmask $service"
            systemctl enable "$service" >/dev/null 2>&1 ||
                rollback_error "cannot enable $service"
            ;;
        enabled-runtime)
            systemctl unmask "$service" >/dev/null 2>&1 ||
                rollback_error "cannot unmask $service"
            systemctl enable --runtime "$service" >/dev/null 2>&1 ||
                rollback_error "cannot enable $service at runtime"
            ;;
        disabled)
            systemctl unmask "$service" >/dev/null 2>&1 ||
                rollback_error "cannot unmask $service"
            systemctl disable "$service" >/dev/null 2>&1 ||
                rollback_error "cannot disable $service"
            ;;
        masked)
            systemctl unmask "$service" >/dev/null 2>&1 ||
                rollback_error "cannot temporarily unmask $service"
            remask=persistent
            ;;
        masked-runtime)
            systemctl unmask "$service" >/dev/null 2>&1 ||
                rollback_error "cannot temporarily unmask $service"
            remask=runtime
            ;;
        static|indirect|generated|transient|not-found) ;;
        *) rollback_error "invalid enablement state for $service" ;;
    esac
    case "$active" in
        active)
            [ "$enabled" = not-found ] ||
                systemctl restart "$service" >/dev/null 2>&1 ||
                rollback_error "cannot restart $service"
            ;;
        inactive)
            [ "$enabled" = not-found ] ||
                systemctl stop "$service" >/dev/null 2>&1 ||
                rollback_error "cannot stop $service"
            ;;
        *) rollback_error "invalid activity state for $service" ;;
    esac
    case "$remask" in
        persistent)
            systemctl mask "$service" >/dev/null 2>&1 ||
                rollback_error "cannot mask $service"
            ;;
        runtime)
            systemctl mask --runtime "$service" >/dev/null 2>&1 ||
                rollback_error "cannot mask $service at runtime"
            ;;
    esac
}

rollback_transaction() {
    local index
    load_transaction
    for index in "${!ITEM_KEYS[@]}"; do
        restore_item "${ITEM_KEYS[$index]}" "${ITEM_PATHS[$index]}" \
            "${ITEM_TYPES[$index]}"
    done
    systemctl daemon-reload >/dev/null 2>&1 ||
        rollback_error "systemd daemon-reload failed"
    if command -v sysctl >/dev/null 2>&1; then
        sysctl --system >/dev/null 2>&1 ||
            rollback_error "sysctl restore failed"
    fi
    for service in "${SERVICES[@]}"; do
        restore_service "$service"
    done
    if [ "$ROLLBACK_FAILED" -ne 0 ]; then
        rollback_error "rollback incomplete; backup retained at $TRANSACTION"
        return 1
    fi
    retire_transaction restored
}

commit_transaction() {
    load_transaction
    retire_transaction deployed
    [ ! -e "$TRANSACTION" ] && [ ! -L "$TRANSACTION" ] ||
        fail "committed backup still exists"
}

case "$ACTION" in
    begin) begin_transaction ;;
    commit) commit_transaction ;;
    rollback) rollback_transaction ;;
esac
