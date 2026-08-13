#!/bin/bash
set -euo pipefail

# Hamn deliberately uses the Docker daemon supplied by the immutable guest
# image. It never installs a host Docker CLI or creates a Docker shim.
SYSTEMCTL=${HAMN_SYSTEMCTL:-systemctl}
DOCKERD=${HAMN_DOCKERD:-dockerd}
DOCKER=${HAMN_DOCKER:-docker}
IP=${HAMN_IP:-ip}
DOCKER_SERVICE=${HAMN_DOCKER_SERVICE:-docker.service}
CONTAINERD_SOCKET=${HAMN_CONTAINERD_SOCKET:-/run/containerd/containerd.sock}
CONFIG=${HAMN_DOCKER_CONFIG:-/etc/docker/daemon.json}
DROPIN_DIR=${HAMN_DOCKER_DROPIN_DIR:-/etc/systemd/system/docker.service.d}
DROPIN=${HAMN_DOCKER_DROPIN:-$DROPIN_DIR/10-hamn-containerd.conf}
DNSMASQ=${HAMN_DNSMASQ:-dnsmasq}
HOST_DNS_SERVICE=${HAMN_HOST_DNS_SERVICE:-hamn-host-dns.service}
HOST_DNS_CONFIG=${HAMN_HOST_DNS_CONFIG:-/etc/dnsmasq.d/hamn-host-dns.conf}
HOST_DNS_UNIT=${HAMN_HOST_DNS_UNIT:-/etc/systemd/system/$HOST_DNS_SERVICE}
PYTHON3=${HAMN_PYTHON3:-python3}
EXTRA_JSON=${HAMN_DOCKER_EXTRA_JSON:-}

atomic_replace() {
    local source=$1 destination=$2
    if mv -fT -- "$source" "$destination" 2>/dev/null; then
        return 0
    fi
    mv -fh -- "$source" "$destination"
}

replace_if_changed() {
    local source=$1 destination=$2
    if [ -f "$destination" ] && cmp -s "$source" "$destination"; then
        rm -f -- "$source"
        return 1
    fi
    chmod 0644 "$source"
    atomic_replace "$source" "$destination"
    return 0
}

gateway_address() {
    local gateway
    gateway=$("$IP" -4 route show default 2>/dev/null |
        awk '$1 == "default" { for (i = 1; i < NF; i++) if ($i == "via") { print $(i + 1); exit } }')
    case "$gateway" in
        [0-9]*.[0-9]*.[0-9]*.[0-9]*) printf '%s\n' "$gateway" ;;
        *) return 1 ;;
    esac
}

DOCKERD_BIN=$(command -v "$DOCKERD" 2>/dev/null || true)
if [ -z "$DOCKERD_BIN" ] || [ ! -x "$DOCKERD_BIN" ]; then
    echo "hamn: Docker Engine is missing from the guest image; install a Hamn Docker guest image" >&2
    exit 1
fi
if [ ! -S "$CONTAINERD_SOCKET" ]; then
    echo "hamn: system containerd socket is unavailable: $CONTAINERD_SOCKET" >&2
    exit 1
fi
command -v "$DOCKER" >/dev/null 2>&1 || {
    echo "hamn: Docker client is missing from the guest image" >&2
    exit 1
}
command -v "$PYTHON3" >/dev/null 2>&1 || {
    echo "hamn: Python 3 is missing from the guest image; it is required to validate Docker daemon settings" >&2
    exit 1
}
DNSMASQ_BIN=$(command -v "$DNSMASQ" 2>/dev/null || true)
if [ -z "$DNSMASQ_BIN" ] || [ ! -x "$DNSMASQ_BIN" ]; then
    echo "hamn: dnsmasq is missing from the guest image; it provides host.docker.internal" >&2
    exit 1
fi

gateway=$(gateway_address || true)
if [ -z "$gateway" ]; then
    echo "hamn: cannot discover the guest gateway for host.docker.internal" >&2
    exit 1
fi

install -d -m 0755 "$(dirname "$CONFIG")" "$DROPIN_DIR" \
    "$(dirname "$HOST_DNS_CONFIG")" "$(dirname "$HOST_DNS_UNIT")"

# Docker Engine does not create host.docker.internal on a generic Linux host.
# The guest DNS service owns the conventional name for every Docker network.
# host.hamn.internal stays as a one-release compatibility alias.
host_dns_config_tmp=$(mktemp "${HOST_DNS_CONFIG}.XXXXXX")
trap 'rm -f "$host_dns_config_tmp"' EXIT
cat >"$host_dns_config_tmp" <<EOF
bind-dynamic
listen-address=172.17.0.1
no-hosts
address=/host.docker.internal/$gateway
address=/host.hamn.internal/$gateway
EOF
host_dns_config_changed=0
if replace_if_changed "$host_dns_config_tmp" "$HOST_DNS_CONFIG"; then
    host_dns_config_changed=1
fi
trap - EXIT

host_dns_unit_tmp=$(mktemp "${HOST_DNS_UNIT}.XXXXXX")
trap 'rm -f "$host_dns_unit_tmp"' EXIT
cat >"$host_dns_unit_tmp" <<EOF
[Unit]
Description=Hamn Docker host-name DNS
After=network-online.target
Wants=network-online.target
Before=docker.service

[Service]
Type=simple
ExecStart=$DNSMASQ_BIN --keep-in-foreground --conf-file=$HOST_DNS_CONFIG
Restart=on-failure
RestartSec=1
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=full

[Install]
WantedBy=multi-user.target
EOF
host_dns_unit_changed=0
if replace_if_changed "$host_dns_unit_tmp" "$HOST_DNS_UNIT"; then
    host_dns_unit_changed=1
fi
trap - EXIT

config_tmp=$(mktemp "${CONFIG}.XXXXXX")
trap 'rm -f "$config_tmp"' EXIT
if ! "$PYTHON3" - "$CONTAINERD_SOCKET" "$gateway" "$EXTRA_JSON" \
    >"$config_tmp" <<'PY'
import json
import sys


def reject_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key: " + key)
        result[key] = value
    return result


def reject_constant(value):
    raise ValueError("non-finite JSON value: " + value)


containerd_socket, gateway, extra_text = sys.argv[1:]
try:
    extra = (json.loads(extra_text, object_pairs_hook=reject_duplicate_keys,
                        parse_constant=reject_constant)
             if extra_text else {})
except (TypeError, ValueError, json.JSONDecodeError) as error:
    print("hamn: docker.daemonJson must be one strict JSON object: " + str(error),
          file=sys.stderr)
    sys.exit(1)

if not isinstance(extra, dict):
    print("hamn: docker.daemonJson must be one JSON object", file=sys.stderr)
    sys.exit(1)

for reserved in ("containerd", "host-gateway-ip", "hosts", "data-root", "exec-root",
                 "dns", "bip", "bridge", "fixed-cidr", "default-address-pools"):
    if reserved in extra:
        print("hamn: docker.daemonJson cannot override Hamn-managed key: " + reserved,
              file=sys.stderr)
        sys.exit(1)

features = extra.pop("features", {})
if not isinstance(features, dict):
    print("hamn: docker.daemonJson.features must be a JSON object", file=sys.stderr)
    sys.exit(1)
if "buildkit" in features and features["buildkit"] is not True:
    print("hamn: docker.daemonJson.features.buildkit must remain true", file=sys.stderr)
    sys.exit(1)
features["buildkit"] = True

extra["containerd"] = containerd_socket
extra["features"] = features
extra["bip"] = "172.17.0.1/16"
extra["dns"] = ["172.17.0.1"]
extra["host-gateway-ip"] = gateway
json.dump(extra, sys.stdout, indent=2, sort_keys=True)
print()
PY
then
    echo "hamn: cannot validate Docker daemon settings" >&2
    exit 1
fi
config_changed=0
if replace_if_changed "$config_tmp" "$CONFIG"; then
    config_changed=1
fi
trap - EXIT

dropin_tmp=$(mktemp "${DROPIN}.XXXXXX")
trap 'rm -f "$dropin_tmp"' EXIT
cat >"$dropin_tmp" <<EOF
[Unit]
After=containerd.service
Requires=containerd.service

[Service]
ExecStart=
ExecStart=$DOCKERD_BIN -H fd:// --containerd=$CONTAINERD_SOCKET
EOF
dropin_changed=0
if replace_if_changed "$dropin_tmp" "$DROPIN"; then
    dropin_changed=1
fi
trap - EXIT

if [ "$dropin_changed" -eq 1 ] || [ "$host_dns_unit_changed" -eq 1 ]; then
    "$SYSTEMCTL" daemon-reload
fi
if ! "$SYSTEMCTL" is-enabled --quiet "$HOST_DNS_SERVICE"; then
    "$SYSTEMCTL" enable "$HOST_DNS_SERVICE"
fi
if "$SYSTEMCTL" is-active --quiet "$HOST_DNS_SERVICE"; then
    if [ "$host_dns_config_changed" -eq 1 ] || [ "$host_dns_unit_changed" -eq 1 ]; then
        "$SYSTEMCTL" restart "$HOST_DNS_SERVICE"
    fi
else
    "$SYSTEMCTL" start "$HOST_DNS_SERVICE"
fi
if ! "$SYSTEMCTL" is-enabled --quiet "$DOCKER_SERVICE"; then
    "$SYSTEMCTL" enable "$DOCKER_SERVICE"
fi
if "$SYSTEMCTL" is-active --quiet "$DOCKER_SERVICE"; then
    if [ "$config_changed" -eq 1 ] || [ "$dropin_changed" -eq 1 ]; then
        "$SYSTEMCTL" restart "$DOCKER_SERVICE"
    fi
else
    "$SYSTEMCTL" start "$DOCKER_SERVICE"
fi

for _ in $(seq 1 50); do
    if "$DOCKER" version --format '{{.Server.Version}}' >/dev/null 2>&1; then
        echo "hamn: warning: host.hamn.internal is a 0.0.1 compatibility alias and will be removed in the next release; use host.docker.internal" >&2
        exit 0
    fi
    sleep 0.1
done
echo "hamn: Docker daemon did not become ready" >&2
exit 1
