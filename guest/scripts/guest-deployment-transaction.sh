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
    hamnd libexec_hamn hamnd_unit k3s_unit etc_hamn containerd_config docker_config
    docker_dropin host_dns_config host_dns_unit modules_config sysctl_config cni_bin
)
ITEM_PATHS=(
    /usr/local/bin/hamnd
    /usr/local/libexec/hamn
    /etc/systemd/system/hamnd.service
    /etc/systemd/system/k3s.service
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
    file directory file file directory file file directory file file file file directory
)
SERVICES=(hamnd.service containerd.service docker.service hamn-host-dns.service k3s.service)

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

refuse_pending_transaction() {
    local entry name phase
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
        phase=
        if [ -f "$entry/phase" ] && [ ! -L "$entry/phase" ]; then
            phase=$(cat "$entry/phase")
        fi
        if [ "$phase" = ready ]; then
            fail "pending deployment transaction $name exists at $entry; "\
"recover it before retrying: sudo bash $RECOVERY_SCRIPT "\
"rollback $name"
        fi
        fail "incomplete deployment transaction $name exists at $entry; "\
"inspect it before retrying"
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
    printf 'ready\n' >"$TRANSACTION/phase" ||
        fail "cannot complete transaction backup"
    chmod 0600 "$TRANSACTION/phase" || fail "cannot secure transaction phase"
    cleanup_incomplete=0
    trap - EXIT
}

load_transaction() {
    [ -d "$TRANSACTION" ] && [ ! -L "$TRANSACTION" ] ||
        fail "transaction does not exist"
    [ -O "$TRANSACTION" ] || fail "transaction has the wrong owner"
    [ -d "$TRANSACTION/data" ] && [ ! -L "$TRANSACTION/data" ] ||
        fail "transaction data is unsafe"
    [ -d "$TRANSACTION/meta" ] && [ ! -L "$TRANSACTION/meta" ] ||
        fail "transaction metadata is unsafe"
    [ -f "$TRANSACTION/phase" ] && [ ! -L "$TRANSACTION/phase" ] ||
        fail "transaction phase is unsafe"
    [ "$(cat "$TRANSACTION/phase")" = ready ] ||
        fail "transaction is incomplete"
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
    rm -rf -- "$TRANSACTION" ||
        fail "rollback succeeded but backup cleanup failed"
}

commit_transaction() {
    load_transaction
    rm -rf -- "$TRANSACTION" || fail "cannot remove committed backup"
    [ ! -e "$TRANSACTION" ] && [ ! -L "$TRANSACTION" ] ||
        fail "committed backup still exists"
}

case "$ACTION" in
    begin) begin_transaction ;;
    commit) commit_transaction ;;
    rollback) rollback_transaction ;;
esac
