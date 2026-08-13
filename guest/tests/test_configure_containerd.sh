#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
SOURCE="$WORK/source"
DEST="$WORK/dest"
mkdir -p "$SOURCE" "$DEST"

plugins=(bridge host-local loopback portmap firewall tuning)
for plugin in "${plugins[@]}"; do
    printf '#!/bin/sh\nexit 0\n' >"$SOURCE/$plugin"
    chmod +x "$SOURCE/$plugin"
done

HAMN_CNI_SOURCE_DIR="$SOURCE" HAMN_CNI_BIN_DIR="$DEST" \
    bash "$ROOT/scripts/configure-containerd.sh" --ensure-cni
for plugin in "${plugins[@]}"; do
    [ -x "$DEST/$plugin" ]
    [ "$(readlink "$DEST/$plugin")" = "$SOURCE/$plugin" ]
done

rm "$DEST/bridge"
mkdir "$DEST/bridge"
if HAMN_CNI_SOURCE_DIR="$SOURCE" HAMN_CNI_BIN_DIR="$DEST" \
    bash "$ROOT/scripts/configure-containerd.sh" --ensure-cni \
    2>"$WORK/destination-directory.error"; then
    echo "FAIL: CNI destination directory was accepted as a plugin" >&2
    exit 1
fi
grep -q 'CNI plugin destination is a directory: bridge' \
    "$WORK/destination-directory.error"
rmdir "$DEST/bridge"

rm "$DEST/portmap"
HAMN_CNI_SOURCE_DIR="$SOURCE" HAMN_CNI_BIN_DIR="$DEST" \
    bash "$ROOT/scripts/configure-containerd.sh" --ensure-cni
[ -x "$DEST/portmap" ]

rm "$DEST/tuning" "$SOURCE/tuning"
mkdir "$SOURCE/tuning"
if HAMN_CNI_SOURCE_DIR="$SOURCE" HAMN_CNI_BIN_DIR="$DEST" \
    bash "$ROOT/scripts/configure-containerd.sh" --ensure-cni 2>"$WORK/error"; then
    echo "FAIL: CNI source directory was accepted as a plugin" >&2
    exit 1
fi
grep -q 'required CNI plugin is not a regular executable file: tuning' \
    "$WORK/error"

assert_original_plugins() {
    case_dest=$1
    case_old=$2
    [ "$(readlink "$case_dest/bridge")" = "$case_old/bridge" ]
    [ "$(cat "$case_dest/host-local")" = original-host-local ]
    [ ! -e "$case_dest/loopback" ] && [ ! -L "$case_dest/loopback" ]
    [ "$(readlink "$case_dest/portmap")" = "$case_old/missing-portmap" ]
    [ "$(cat "$case_dest/firewall")" = original-firewall ]
    [ ! -e "$case_dest/tuning" ] && [ ! -L "$case_dest/tuning" ]
}

for failure_step in 1 2 3 4 5 6; do
    CASE="$WORK/failure-$failure_step"
    CASE_SOURCE="$CASE/source"
    CASE_DEST="$CASE/dest"
    CASE_OLD="$CASE/old"
    mkdir -p "$CASE_SOURCE" "$CASE_DEST" "$CASE_OLD"
    for plugin in "${plugins[@]}"; do
        printf '#!/bin/sh\nexit 0\n' >"$CASE_SOURCE/$plugin"
        chmod +x "$CASE_SOURCE/$plugin"
    done
    printf '#!/bin/sh\nexit 0\n' >"$CASE_OLD/bridge"
    chmod 0644 "$CASE_OLD/bridge"
    ln -s "$CASE_OLD/bridge" "$CASE_DEST/bridge"
    printf '%s\n' original-host-local >"$CASE_DEST/host-local"
    chmod 0640 "$CASE_DEST/host-local"
    ln -s "$CASE_OLD/missing-portmap" "$CASE_DEST/portmap"
    printf '%s\n' original-firewall >"$CASE_DEST/firewall"
    chmod 0600 "$CASE_DEST/firewall"

    if HAMN_TEST=1 HAMN_TEST_CNI_PLUGIN_FAILURE_AT="$failure_step" \
        HAMN_CNI_SOURCE_DIR="$CASE_SOURCE" HAMN_CNI_BIN_DIR="$CASE_DEST" \
        bash "$ROOT/scripts/configure-containerd.sh" --ensure-cni \
        2>"$CASE/error"; then
        echo "FAIL: injected CNI failure $failure_step was accepted" >&2
        exit 1
    fi
    grep -q "injected CNI plugin failure at step $failure_step" \
        "$CASE/error"
    assert_original_plugins "$CASE_DEST" "$CASE_OLD"
done

FULL="$WORK/full"
FULL_BIN="$FULL/bin"
FULL_STATE="$FULL/state"
FULL_ETC="$FULL/etc"
FULL_CNI_SOURCE="$FULL/cni-source"
FULL_CNI_DEST="$FULL/cni-dest"
mkdir -p "$FULL_BIN" "$FULL_STATE" "$FULL_ETC" \
    "$FULL_CNI_SOURCE" "$FULL_CNI_DEST"
for plugin in "${plugins[@]}"; do
    printf '#!/bin/sh\nexit 0\n' >"$FULL_CNI_SOURCE/$plugin"
    chmod +x "$FULL_CNI_SOURCE/$plugin"
done

FULL_SYSTEMCTL_LOG="$FULL/systemctl.log"
cat >"$FULL_BIN/systemctl" <<'EOF'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >>"$HAMN_TEST_SYSTEMCTL_LOG"
action=$1
service=${3:-${2:-}}
case "$action" in
is-enabled) [ -f "$HAMN_TEST_SYSTEMCTL_STATE/enabled-$service" ] ;;
is-active) [ -f "$HAMN_TEST_SYSTEMCTL_STATE/active-$service" ] ;;
enable) touch "$HAMN_TEST_SYSTEMCTL_STATE/enabled-$service" ;;
start) touch "$HAMN_TEST_SYSTEMCTL_STATE/active-$service" ;;
restart)
    [ "${HAMN_TEST_RESTART_FAIL:-0}" != 1 ] || exit 43
    touch "$HAMN_TEST_SYSTEMCTL_STATE/active-$service"
    ;;
disable) ;;
*) exit 2 ;;
esac
EOF
chmod +x "$FULL_BIN/systemctl"

cat >"$FULL_BIN/containerd" <<'EOF'
#!/bin/bash
set -euo pipefail
[ "$*" = 'config default' ]
cat <<CONFIG
disabled_plugins = ["cri"]
SystemdCgroup = false
sandbox_image = "old.invalid/pause:1"
sandbox = 'old.invalid/pause:1'
# version=${HAMN_TEST_CONFIG_VERSION:-1}
CONFIG
EOF
cat >"$FULL_BIN/ctr" <<'EOF'
#!/bin/sh
echo 'io.containerd.grpc.v1 cri ok'
EOF
for command in runc modprobe sysctl; do
    printf '#!/bin/sh\nexit 0\n' >"$FULL_BIN/$command"
done
chmod +x "$FULL_BIN"/*

export HAMN_SYSTEMCTL="$FULL_BIN/systemctl"
export HAMN_TEST_SYSTEMCTL_LOG="$FULL_SYSTEMCTL_LOG"
export HAMN_TEST_SYSTEMCTL_STATE="$FULL_STATE"
export HAMN_CONTAINERD="$FULL_BIN/containerd"
export HAMN_RUNC="$FULL_BIN/runc"
export HAMN_CTR="$FULL_BIN/ctr"
export HAMN_MODPROBE="$FULL_BIN/modprobe"
export HAMN_SYSCTL="$FULL_BIN/sysctl"
export HAMN_CONTAINERD_MARKER="$FULL_ETC/containerd.marker"
export HAMN_LEGACY_CONTAINERD_MARKER="$FULL_ETC/containerd-kubernetes-v1"
export HAMN_CONTAINERD_CONFIG="$FULL_ETC/config.toml"
export HAMN_MODULES_CONFIG="$FULL_ETC/modules.conf"
export HAMN_SYSCTL_CONFIG="$FULL_ETC/sysctl.conf"
export HAMN_CNI_SOURCE_DIR="$FULL_CNI_SOURCE"
export HAMN_CNI_BIN_DIR="$FULL_CNI_DEST"

: >"$FULL_SYSTEMCTL_LOG"
printf 'obsolete\n' >"$HAMN_LEGACY_CONTAINERD_MARKER"
bash "$ROOT/scripts/configure-containerd.sh"
grep -Fxq 'enable containerd' "$FULL_SYSTEMCTL_LOG"
grep -Fxq 'start containerd' "$FULL_SYSTEMCTL_LOG"
! grep -q '^restart containerd$' "$FULL_SYSTEMCTL_LOG"
[ -f "$HAMN_CONTAINERD_MARKER" ]
[ ! -e "$HAMN_LEGACY_CONTAINERD_MARKER" ]

# Unsafe legacy evidence fails before touching the current marker or services.
mkdir "$HAMN_LEGACY_CONTAINERD_MARKER"
: >"$FULL_SYSTEMCTL_LOG"
if bash "$ROOT/scripts/configure-containerd.sh"; then
    echo "FAIL: configure-containerd accepted a legacy marker directory" >&2
    exit 1
fi
[ -f "$HAMN_CONTAINERD_MARKER" ]
[ ! -s "$FULL_SYSTEMCTL_LOG" ]
rmdir "$HAMN_LEGACY_CONTAINERD_MARKER"

# An identical active configuration performs no service mutation.
: >"$FULL_SYSTEMCTL_LOG"
bash "$ROOT/scripts/configure-containerd.sh"
! grep -Eq '^(enable|start|restart) containerd$' "$FULL_SYSTEMCTL_LOG"

# A config change restarts once. Failure removes the applied marker so an
# identical retry still restarts and completes the interrupted transition.
: >"$FULL_SYSTEMCTL_LOG"
HAMN_TEST_CONFIG_VERSION=2 bash "$ROOT/scripts/configure-containerd.sh"
[ "$(grep -c '^restart containerd$' "$FULL_SYSTEMCTL_LOG")" -eq 1 ]
if HAMN_TEST_CONFIG_VERSION=3 HAMN_TEST_RESTART_FAIL=1 \
    bash "$ROOT/scripts/configure-containerd.sh"; then
    echo "FAIL: containerd restart failure was hidden" >&2
    exit 1
fi
[ ! -e "$HAMN_CONTAINERD_MARKER" ]
: >"$FULL_SYSTEMCTL_LOG"
HAMN_TEST_CONFIG_VERSION=3 bash "$ROOT/scripts/configure-containerd.sh"
grep -Fxq 'restart containerd' "$FULL_SYSTEMCTL_LOG"
[ -f "$HAMN_CONTAINERD_MARKER" ]

# An inactive unchanged service is started, not restarted.
rm "$FULL_STATE/active-containerd"
: >"$FULL_SYSTEMCTL_LOG"
HAMN_TEST_CONFIG_VERSION=3 bash "$ROOT/scripts/configure-containerd.sh"
grep -Fxq 'start containerd' "$FULL_SYSTEMCTL_LOG"
! grep -Fxq 'restart containerd' "$FULL_SYSTEMCTL_LOG"

echo "PASS: system containerd enables CRI and repairs CNI atomically"
