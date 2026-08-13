#!/bin/bash
set -euo pipefail

K3S_CNI_CONFIG_SOURCE=${HAMN_K3S_CNI_CONFIG_SOURCE:-/var/lib/rancher/k3s/agent/etc/cni/net.d/10-flannel.conflist}
K3S_FLANNEL_SOURCE=${HAMN_K3S_FLANNEL_SOURCE:-/var/lib/rancher/k3s/data/cni/flannel}
K3S_BANDWIDTH_SOURCE=${HAMN_K3S_BANDWIDTH_SOURCE:-/var/lib/rancher/k3s/data/cni/bandwidth}
CNI_CONFIG_LINK=${HAMN_CNI_CONFIG_LINK:-/etc/cni/net.d/10-flannel.conflist}
FLANNEL_LINK=${HAMN_FLANNEL_LINK:-/opt/cni/bin/flannel}
BANDWIDTH_LINK=${HAMN_BANDWIDTH_LINK:-/opt/cni/bin/bandwidth}
CONFIGURE_CONTAINERD=${HAMN_CONFIGURE_CONTAINERD:-/usr/local/libexec/hamn/configure-containerd}
INSTALL_K3S=${HAMN_INSTALL_K3S:-/usr/local/libexec/hamn/install-k3s}
K3S_BIN=${HAMN_K3S_BIN:-/usr/local/bin/k3s}
CURL=${HAMN_CURL:-curl}
CNI_TRANSACTION_DIR=${HAMN_K3S_CNI_TRANSACTION_DIR:-/var/lib/hamn/k3s-cni-transaction}

cni_changed=false
cni_link_step=0
cni_snapshot_dir=

atomic_replace() {
    source=$1
    destination=$2
    if mv -fT -- "$source" "$destination" 2>/dev/null; then
        return 0
    fi
    mv -fh -- "$source" "$destination"
}

k3s_active_state() {
    if systemctl is-active --quiet k3s.service; then
        printf '1\n'
    else
        rc=$?
        [ "$rc" -eq 3 ] || return "$rc"
        printf '0\n'
    fi
}

k3s_enabled_state() {
    if systemctl is-enabled --quiet k3s.service; then
        printf '1\n'
    else
        rc=$?
        [ "$rc" -eq 1 ] || return "$rc"
        printf '0\n'
    fi
}

restore_k3s_state() {
    active=$1
    enabled=$2
    failed=false
    if [ "$enabled" = 1 ]; then
        systemctl enable k3s.service || failed=true
    else
        systemctl disable k3s.service || failed=true
    fi
    if [ "$active" = 1 ]; then
        systemctl start k3s.service || failed=true
    else
        systemctl stop k3s.service || failed=true
    fi
    if ! actual_active=$(k3s_active_state) ||
       ! actual_enabled=$(k3s_enabled_state) ||
       [ "$actual_active" != "$active" ] ||
       [ "$actual_enabled" != "$enabled" ]; then
        failed=true
    fi
    [ "$failed" = false ]
}

rollback_failed_start() {
    rc=$?
    trap - EXIT
    set +e
    cni_restored=false
    state_restored=false
    if restore_cni_links true; then
        cni_restored=true
    else
        echo "hamn: cannot restore the previous CNI paths" >&2
    fi
    if [ "$cni_restored" = true ] &&
       restore_k3s_state "$was_active" "$was_enabled"; then
        state_restored=true
    else
        echo "hamn: cannot restore the previous k3s service state" >&2
    fi
    if [ "$cni_restored" = true ] && [ "$state_restored" = true ]; then
        discard_cni_snapshot ||
            echo "hamn: cannot discard recovered CNI snapshots" >&2
    fi
    exit "$rc"
}

snapshot_cni_path() {
    path=$1
    backup=$2
    if [ -L "$path" ] || [ -f "$path" ]; then
        cp -aP -- "$path" "$backup"
    elif [ -d "$path" ]; then
        echo "hamn: CNI link destination is a directory: $path" >&2
        return 1
    elif [ -e "$path" ]; then
        echo "hamn: unsupported CNI destination type: $path" >&2
        return 1
    else
        : >"$backup.absent"
    fi
}

snapshot_cni_links() {
    active=${1:-}
    enabled=${2:-}
    parent=$(dirname "$CNI_TRANSACTION_DIR")
    preparing="$CNI_TRANSACTION_DIR.preparing"
    install -d -m 0755 "$parent"
    rm -rf -- "$preparing"
    mkdir "$preparing"
    index=0
    for path in "$CNI_CONFIG_LINK" "$FLANNEL_LINK" "$BANDWIDTH_LINK"; do
        if ! snapshot_cni_path "$path" "$preparing/$index"; then
            rm -rf -- "$preparing"
            return 1
        fi
        index=$((index + 1))
    done
    if [ -n "$active" ] || [ -n "$enabled" ]; then
        if { [ "$active" != 0 ] && [ "$active" != 1 ]; } ||
           { [ "$enabled" != 0 ] && [ "$enabled" != 1 ]; }; then
            rm -rf -- "$preparing"
            return 1
        fi
        printf '%s %s\n' "$active" "$enabled" >"$preparing/service-state"
    fi
    if ! mv -- "$preparing" "$CNI_TRANSACTION_DIR"; then
        rm -rf -- "$preparing"
        return 1
    fi
    cni_snapshot_dir=$CNI_TRANSACTION_DIR
    sync
}

restore_cni_path() {
    path=$1
    backup=$2
    temporary="$path.hamn-restore.$$"
    rm -f -- "$temporary" || return 1
    if [ -L "$backup" ] || [ -f "$backup" ]; then
        if ! cp -aP -- "$backup" "$temporary" ||
           ! atomic_replace "$temporary" "$path"; then
            rm -f -- "$temporary"
            return 1
        fi
    elif [ -f "$backup.absent" ]; then
        rm -f -- "$path"
    else
        echo "hamn: missing CNI path snapshot: $path" >&2
        return 1
    fi
}

discard_cni_snapshot() {
    [ -n "$cni_snapshot_dir" ] || return 0
    sync
    rm -rf -- "$cni_snapshot_dir"
    sync
    cni_snapshot_dir=
}

restore_cni_links() {
    retain_snapshot=${1:-false}
    [ -n "$cni_snapshot_dir" ] || return 0
    failed=false
    index=0
    for path in "$CNI_CONFIG_LINK" "$FLANNEL_LINK" "$BANDWIDTH_LINK"; do
        restore_cni_path "$path" "$cni_snapshot_dir/$index" || failed=true
        index=$((index + 1))
    done
    if [ "$failed" = true ]; then
        echo "hamn: CNI snapshots retained at $cni_snapshot_dir" >&2
        return 1
    fi
    sync
    [ "$retain_snapshot" = true ] || discard_cni_snapshot
}

recover_cni_transaction() {
    [ -d "$CNI_TRANSACTION_DIR" ] || return 0
    cni_snapshot_dir=$CNI_TRANSACTION_DIR
    active=
    enabled=
    if [ -f "$cni_snapshot_dir/service-state" ]; then
        read -r active enabled extra <"$cni_snapshot_dir/service-state" ||
            return 1
        if [ -n "${extra:-}" ] ||
           { [ "$active" != 0 ] && [ "$active" != 1 ]; } ||
           { [ "$enabled" != 0 ] && [ "$enabled" != 1 ]; }; then
            echo "hamn: invalid k3s CNI recovery state" >&2
            return 1
        fi
        systemctl stop k3s.service || return 1
    fi
    restore_cni_links true || return 1
    if [ -n "$active" ] && ! restore_k3s_state "$active" "$enabled"; then
        return 1
    fi
    discard_cni_snapshot
}

ensure_symlink() {
    source=$1
    destination=$2
    cni_link_step=$((cni_link_step + 1))
    if [ "${HAMN_TEST:-0}" = 1 ] &&
       [ "${HAMN_TEST_CNI_LINK_FAILURE_AT:-0}" = "$cni_link_step" ]; then
        echo "hamn: injected CNI link failure at step $cni_link_step" >&2
        return 1
    fi
    if [ -L "$destination" ] && [ "$(readlink "$destination")" = "$source" ]; then
        return 0
    fi
    if [ -d "$destination" ] && [ ! -L "$destination" ]; then
        echo "hamn: CNI link destination is a directory: $destination" >&2
        return 1
    fi
    temporary="$destination.hamn-install.$$"
    rm -f -- "$temporary" || return 1
    ln -s -- "$source" "$temporary" || return 1
    if ! atomic_replace "$temporary" "$destination"; then
        rm -f -- "$temporary"
        return 1
    fi
    if [ "${HAMN_TEST:-0}" = 1 ] &&
       [ "${HAMN_TEST_CNI_LINK_KILL_AFTER:-0}" = "$cni_link_step" ]; then
        kill -KILL "$$"
    fi
    cni_changed=true
}

repair_cni_links() {
    retain_snapshot=${1:-false}
    snapshot_active=${2:-}
    snapshot_enabled=${3:-}
    if [ ! -f "$K3S_CNI_CONFIG_SOURCE" ] ||
       [ ! -f "$K3S_FLANNEL_SOURCE" ] ||
       [ ! -x "$K3S_FLANNEL_SOURCE" ] ||
       [ ! -f "$K3S_BANDWIDTH_SOURCE" ] ||
       [ ! -x "$K3S_BANDWIDTH_SOURCE" ]; then
        echo "hamn: k3s external CNI assets are incomplete" >&2
        return 1
    fi
    install -d -m 0755 "$(dirname "$CNI_CONFIG_LINK")" \
        "$(dirname "$FLANNEL_LINK")" "$(dirname "$BANDWIDTH_LINK")"
    snapshot_cni_links "$snapshot_active" "$snapshot_enabled"
    if ! ensure_symlink "$K3S_CNI_CONFIG_SOURCE" "$CNI_CONFIG_LINK" ||
       ! ensure_symlink "$K3S_FLANNEL_SOURCE" "$FLANNEL_LINK" ||
       ! ensure_symlink "$K3S_BANDWIDTH_SOURCE" "$BANDWIDTH_LINK" ||
       [ "$(readlink "$CNI_CONFIG_LINK")" != "$K3S_CNI_CONFIG_SOURCE" ] ||
       [ "$(readlink "$FLANNEL_LINK")" != "$K3S_FLANNEL_SOURCE" ] ||
       [ "$(readlink "$BANDWIDTH_LINK")" != "$K3S_BANDWIDTH_SOURCE" ] ||
       [ ! -f "$CNI_CONFIG_LINK" ] || [ ! -f "$FLANNEL_LINK" ] ||
       [ ! -x "$FLANNEL_LINK" ] || [ ! -f "$BANDWIDTH_LINK" ] ||
       [ ! -x "$BANDWIDTH_LINK" ]; then
        restore_cni_links || true
        return 1
    fi
    [ "$retain_snapshot" = true ] || discard_cni_snapshot
}

coredns_ready() {
    pod_ips=$("$K3S_BIN" kubectl -n kube-system get pods \
        --selector k8s-app=kube-dns --field-selector status.phase=Running \
        --output jsonpath='{range .items[*]}{.status.podIP}{"\n"}{end}' \
        2>/dev/null) || return 1
    [ -n "$pod_ips" ] || return 1
    for pod_ip in $pod_ips; do
        if "$CURL" --fail --silent --show-error --max-time 1 \
            "http://$pod_ip:8181/ready" >/dev/null 2>&1; then
            return 0
        fi
    done
    return 1
}

action=${1:-start}
recover_cni_transaction
case "$action" in
start)
    "$CONFIGURE_CONTAINERD" --ensure-cni
    if ! ctr plugins ls 2>/dev/null | awk '($1 ~ /cri/ || $2 ~ /cri/) && $NF == "ok" { found=1 } END { exit !found }'; then
        "$CONFIGURE_CONTAINERD"
    fi
    "$INSTALL_K3S"
    if ! was_active=$(k3s_active_state) ||
       ! was_enabled=$(k3s_enabled_state); then
        echo "hamn: cannot determine the current k3s service state" >&2
        exit 1
    fi
    trap rollback_failed_start EXIT
    systemctl enable --now k3s.service
    ready=false
    for _ in $(seq 1 120); do
        if [ -f "$K3S_CNI_CONFIG_SOURCE" ] &&
           [ -f "$K3S_FLANNEL_SOURCE" ] &&
           [ -x "$K3S_FLANNEL_SOURCE" ] &&
           [ -f "$K3S_BANDWIDTH_SOURCE" ] &&
           [ -x "$K3S_BANDWIDTH_SOURCE" ]; then
            ready=true
            break
        fi
        sleep 0.25
    done
    if [ "$ready" != true ]; then
        echo "hamn: k3s did not publish its external CNI assets" >&2
        exit 1
    fi
    repair_cni_links true "$was_active" "$was_enabled"
    rm -f /etc/hamn/k3s-external-cni-v1
    if [ "$cni_changed" = true ]; then
        systemctl restart containerd.service
    fi
    node_ready=false
    for _ in $(seq 1 240); do
        if "$K3S_BIN" kubectl get node hamn \
            --no-headers 2>/dev/null | awk '$2 == "Ready" { ready=1 } END { exit !ready }'; then
            node_ready=true
            break
        fi
        sleep 0.5
    done
    if [ "$node_ready" != true ]; then
        echo "hamn: k3s node did not become Ready" >&2
        systemctl status k3s.service --no-pager >&2 || true
        exit 1
    fi
    if [ "$was_active" = 0 ]; then
        coredns_restarted=false
        for _ in $(seq 1 240); do
            if "$K3S_BIN" kubectl -n kube-system rollout restart \
                deployment/coredns >/dev/null 2>&1; then
                coredns_restarted=true
                break
            fi
            sleep 0.5
        done
        if [ "$coredns_restarted" != true ] ||
           ! "$K3S_BIN" kubectl -n kube-system rollout status \
               deployment/coredns --timeout=120s >/dev/null; then
            echo "hamn: cannot refresh CoreDNS after k3s restart" >&2
            exit 1
        fi
    fi
    for _ in $(seq 1 240); do
        if coredns_ready; then
            discard_cni_snapshot
            trap - EXIT
            exit 0
        fi
        sleep 0.5
    done
    echo "hamn: k3s cluster DNS did not become ready" >&2
    systemctl status k3s.service --no-pager >&2 || true
    exit 1
    ;;
stop)
    systemctl disable --now k3s.service
    ;;
delete)
    systemctl disable --now k3s.service
    rm -rf -- /etc/rancher/k3s /var/lib/rancher/k3s
    ;;
status)
    systemctl is-active k3s.service
    "$K3S_BIN" kubectl get nodes -o wide
    ;;
lifecycle-state)
    if ! active=$(k3s_active_state) ||
       ! enabled=$(k3s_enabled_state); then
        echo "hamn: cannot determine the current k3s service state" >&2
        exit 1
    fi
    printf 'active=%s enabled=%s\n' "$active" "$enabled"
    ;;
restore-state)
    if [ "$#" -ne 3 ] ||
       { [ "$2" != 0 ] && [ "$2" != 1 ]; } ||
       { [ "$3" != 0 ] && [ "$3" != 1 ]; }; then
        echo "usage: configure-k3s restore-state 0|1 0|1" >&2
        exit 2
    fi
    if ! restore_k3s_state "$2" "$3"; then
        echo "hamn: cannot restore the requested k3s service state" >&2
        exit 1
    fi
    ;;
repair-cni)
    repair_cni_links
    ;;
*)
    echo "usage: configure-k3s start|stop|delete|status|lifecycle-state|" \
        "restore-state|repair-cni" >&2
    exit 2
    ;;
esac
