#!/bin/bash
set -euo pipefail

MARKER=${HAMN_CONTAINERD_MARKER:-/etc/hamn/containerd-kubernetes-v2}
LEGACY_MARKER=${HAMN_LEGACY_CONTAINERD_MARKER:-/etc/hamn/containerd-kubernetes-v1}
CONFIG=${HAMN_CONTAINERD_CONFIG:-/etc/containerd/config.toml}
MODULES_CONFIG=${HAMN_MODULES_CONFIG:-/etc/modules-load.d/hamn-kubernetes.conf}
SYSCTL_CONFIG=${HAMN_SYSCTL_CONFIG:-/etc/sysctl.d/99-hamn-kubernetes.conf}
CNI_BIN_DIR=${HAMN_CNI_BIN_DIR:-/opt/cni/bin}
CNI_SOURCE_DIR=${HAMN_CNI_SOURCE_DIR:-/usr/lib/cni}
REQUIRED_CNI_PLUGINS="bridge host-local loopback portmap firewall tuning"
SYSTEMCTL=${HAMN_SYSTEMCTL:-systemctl}
CONTAINERD=${HAMN_CONTAINERD:-containerd}
RUNC=${HAMN_RUNC:-runc}
CTR=${HAMN_CTR:-ctr}
MODPROBE=${HAMN_MODPROBE:-modprobe}
SYSCTL=${HAMN_SYSCTL:-sysctl}

atomic_replace() {
    source=$1
    destination=$2
    if mv -fT -- "$source" "$destination" 2>/dev/null; then
        return 0
    fi
    mv -fh -- "$source" "$destination"
}

validate_legacy_marker() {
    if [ ! -e "$LEGACY_MARKER" ] && [ ! -L "$LEGACY_MARKER" ]; then
        return 0
    fi
    if { [ ! -f "$LEGACY_MARKER" ] && [ ! -L "$LEGACY_MARKER" ]; } ||
       { [ -d "$LEGACY_MARKER" ] && [ ! -L "$LEGACY_MARKER" ]; }; then
        echo "hamn: refusing unsafe legacy containerd marker" >&2
        return 1
    fi
}

remove_legacy_marker() {
    validate_legacy_marker
    if [ ! -e "$LEGACY_MARKER" ] && [ ! -L "$LEGACY_MARKER" ]; then
        return 0
    fi
    rm -f -- "$LEGACY_MARKER"
}

snapshot_cni_plugin() {
    path=$1
    backup=$2
    if [ -L "$path" ]; then
        cp -pP -- "$path" "$backup"
    elif [ -f "$path" ]; then
        cp -p -- "$path" "$backup"
    elif [ -e "$path" ]; then
        echo "hamn: unsupported CNI plugin destination: $path" >&2
        return 1
    else
        : >"$backup.absent"
    fi
}

restore_cni_plugins() {
    snapshot=$1
    failed=0
    for plugin in $REQUIRED_CNI_PLUGINS; do
        destination="$CNI_BIN_DIR/$plugin"
        backup="$snapshot/$plugin"
        temporary="$destination.hamn-restore.$$"
        rm -f -- "$temporary" || failed=1
        if [ -f "$backup.absent" ]; then
            rm -f -- "$destination" || failed=1
        elif [ -L "$backup" ] || [ -f "$backup" ]; then
            if cp -pP -- "$backup" "$temporary" &&
               atomic_replace "$temporary" "$destination"; then
                :
            else
                rm -f -- "$temporary"
                failed=1
            fi
        else
            echo "hamn: missing CNI plugin snapshot: $plugin" >&2
            failed=1
        fi
    done
    return "$failed"
}

replace_cni_plugin() {
    source=$1
    destination=$2
    temporary="$destination.hamn-install.$$"
    rm -f -- "$temporary" || return 1
    ln -s -- "$source" "$temporary" || return 1
    if ! atomic_replace "$temporary" "$destination"; then
        rm -f -- "$temporary"
        return 1
    fi
}

ensure_cni_plugins() {
    install -d -m 0755 "$CNI_BIN_DIR"
    snapshot=$(mktemp -d)
    for plugin in $REQUIRED_CNI_PLUGINS; do
        destination="$CNI_BIN_DIR/$plugin"
        source="$CNI_SOURCE_DIR/$plugin"
        if [ ! -f "$source" ] || [ ! -x "$source" ]; then
            echo "hamn: required CNI plugin is not a regular executable file: $plugin" >&2
            rm -rf -- "$snapshot"
            return 1
        fi
        if [ -d "$destination" ] && [ ! -L "$destination" ]; then
            echo "hamn: CNI plugin destination is a directory: $plugin" >&2
            rm -rf -- "$snapshot"
            return 1
        fi
        if ! snapshot_cni_plugin "$destination" "$snapshot/$plugin"; then
            rm -rf -- "$snapshot"
            return 1
        fi
    done
    step=0
    failed=0
    for plugin in $REQUIRED_CNI_PLUGINS; do
        destination="$CNI_BIN_DIR/$plugin"
        if [ -f "$destination" ] && [ -x "$destination" ]; then
            continue
        fi
        step=$((step + 1))
        if [ "${HAMN_TEST:-0}" = 1 ] &&
           [ "${HAMN_TEST_CNI_PLUGIN_FAILURE_AT:-0}" = "$step" ]; then
            echo "hamn: injected CNI plugin failure at step $step" >&2
            failed=1
            break
        fi
        if ! replace_cni_plugin "$CNI_SOURCE_DIR/$plugin" "$destination"; then
            failed=1
            break
        fi
    done
    for plugin in $REQUIRED_CNI_PLUGINS; do
        [ "$failed" -eq 0 ] || break
        if [ ! -f "$CNI_BIN_DIR/$plugin" ] ||
           [ ! -x "$CNI_BIN_DIR/$plugin" ]; then
            echo "hamn: installed CNI plugin is not a regular executable file: $plugin" >&2
            failed=1
        fi
    done
    if [ "$failed" -ne 0 ]; then
        if ! restore_cni_plugins "$snapshot"; then
            echo "hamn: CNI plugin rollback failed; snapshots retained at $snapshot" >&2
        else
            rm -rf -- "$snapshot"
        fi
        return 1
    fi
    rm -rf -- "$snapshot"
}

case "${1:-}" in
--ensure-cni)
    ensure_cni_plugins
    exit 0
    ;;
"")
    ;;
*)
    echo "usage: configure-containerd [--ensure-cni]" >&2
    exit 2
    ;;
esac

validate_legacy_marker
command -v "$CONTAINERD" >/dev/null
command -v "$RUNC" >/dev/null
ensure_cni_plugins

install -d -m 0755 "$(dirname "$MARKER")" "$(dirname "$CONFIG")" \
    "$(dirname "$MODULES_CONFIG")" "$(dirname "$SYSCTL_CONFIG")"

replace_config() {
    temporary=$1
    destination=$2
    if [ -f "$destination" ] && cmp -s "$temporary" "$destination"; then
        rm -f -- "$temporary"
    else
        chmod 0644 "$temporary"
        atomic_replace "$temporary" "$destination"
    fi
}

modules_tmp=$(mktemp "${MODULES_CONFIG}.XXXXXX")
printf 'overlay\nbr_netfilter\n' >"$modules_tmp"
replace_config "$modules_tmp" "$MODULES_CONFIG"
"$MODPROBE" overlay
"$MODPROBE" br_netfilter

sysctl_tmp=$(mktemp "${SYSCTL_CONFIG}.XXXXXX")
cat >"$sysctl_tmp" <<'EOF'
net.ipv4.ip_forward = 1
net.bridge.bridge-nf-call-iptables = 1
net.bridge.bridge-nf-call-ip6tables = 1
EOF
replace_config "$sysctl_tmp" "$SYSCTL_CONFIG"
"$SYSCTL" --system >/dev/null

containerd_tmp=$(mktemp "${CONFIG}.XXXXXX")
trap 'rm -f "$containerd_tmp"' EXIT
"$CONTAINERD" config default >"$containerd_tmp"
sed -i.bak '/disabled_plugins.*cri/d' "$containerd_tmp"
sed -i.bak 's/SystemdCgroup = false/SystemdCgroup = true/g' "$containerd_tmp"
sed -i.bak 's#sandbox_image = "[^"]*"#sandbox_image = "registry.k8s.io/pause:3.10"#g' "$containerd_tmp"
sed -i.bak "s#sandbox = '[^']*'#sandbox = 'registry.k8s.io/pause:3.10.1'#g" "$containerd_tmp"
rm -f -- "${containerd_tmp}.bak"
containerd_changed=0
if [ ! -f "$CONFIG" ] || ! cmp -s "$containerd_tmp" "$CONFIG"; then
    rm -f -- "$MARKER"
    chmod 0644 "$containerd_tmp"
    atomic_replace "$containerd_tmp" "$CONFIG"
    containerd_changed=1
else
    rm -f -- "$containerd_tmp"
fi
trap - EXIT
[ -f "$MARKER" ] || containerd_changed=1

if ! "$SYSTEMCTL" is-enabled --quiet containerd; then
    "$SYSTEMCTL" enable containerd
fi
if "$SYSTEMCTL" is-active --quiet containerd; then
    if [ "$containerd_changed" -eq 1 ]; then
        "$SYSTEMCTL" restart containerd
    fi
else
    "$SYSTEMCTL" start containerd
fi
for _ in $(seq 1 50); do
    if "$CTR" plugins ls 2>/dev/null | awk '($1 ~ /cri/ || $2 ~ /cri/) && $NF == "ok" { found=1 } END { exit !found }'; then
        touch "$MARKER"
        remove_legacy_marker
        exit 0
    fi
    sleep 0.1
done
echo "hamn: containerd CRI plugin did not become ready" >&2
exit 1
