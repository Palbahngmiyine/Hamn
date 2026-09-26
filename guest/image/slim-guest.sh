#!/bin/bash
# Runs offline inside the newly provisioned, disposable image only.
set -euo pipefail
export LC_ALL=C

installed() {
    case "$(dpkg-query -W -f='${db:Status-Abbrev}' "$1" 2>/dev/null)" in
        ii*) return 0 ;;
        *) return 1 ;;
    esac
}

# Protect the runtime dependency closure before purging build dependencies.
# gcc/make were installed only for hamnd; nothing compiles at normal VM startup.
# binutils came with gcc; an installed interpreter only suggests it, and
# autoremove keeps suggested packages.
# Hamn's guest tools are C and shell and need no interpreter; packages that
# the base image's own cloud-init depends on stay installed through apt.
apt-mark manual curl docker.io containerd runc \
    containernetworking-plugins qemu-user-static binfmt-support dnsmasq-base nftables
apt-get -y purge gcc make binutils
# Ubuntu Minimal's ubuntu-cloud-minimal holds the base system (cloud-init,
# sshd, sudo, ...) and depends on snapd. Pin everything it holds before it goes
# with snapd and lxd-installer, so autoremove drops only what they alone needed.
if installed ubuntu-cloud-minimal; then
    held=()
    for name in $(dpkg-query -W -f='${Pre-Depends}, ${Depends}, ${Recommends}\n' \
        ubuntu-cloud-minimal | tr ',|' '\n\n' | sed 's/([^)]*)//; s/:any//'); do
        case "$name" in
            ''|snapd|lxd-installer) ;;
            *)
                if installed "$name"; then
                    held+=("$name")
                fi
                ;;
        esac
    done
    [ "${#held[@]}" -gt 0 ]
    apt-mark manual "${held[@]}"
fi
removed=()
for name in snapd lxd-installer ubuntu-cloud-minimal; do
    if installed "$name"; then
        removed+=("$name")
    fi
done
if [ "${#removed[@]}" -gt 0 ]; then
    apt-get -y purge "${removed[@]}"
fi
apt-get -y autoremove --purge
for command in cloud-init sshd sudo netplan git; do
    command -v "$command" >/dev/null
done
# dpkg-excludes keeps later package versions free of these paths; remove what
# the base image's own kernel already installed.
rm -rf /lib/firmware/*/device-tree
test -f /etc/dpkg/dpkg.cfg.d/hamn-excludes
test "$(echo /usr/bin/qemu-*-static)" = /usr/bin/qemu-x86_64-static
test "$(echo /usr/lib/binfmt.d/qemu-*.conf)" = /usr/lib/binfmt.d/qemu-x86_64.conf
test "$(echo /usr/lib/cni/*)" = \
    '/usr/lib/cni/bridge /usr/lib/cni/firewall /usr/lib/cni/host-local /usr/lib/cni/loopback /usr/lib/cni/portmap /usr/lib/cni/tuning'
test ! -e /usr/bin/containerd-stress
# Only hamn-host-dns.service runs dnsmasq. The dnsmasq package's system service
# also reads /etc/dnsmasq.d, where Hamn's bind-dynamic and ubuntu-fan's
# bind-interfaces conflict, so it would fail on every boot after deployment.
if installed dnsmasq; then
    echo "slim-guest: the dnsmasq system service package is installed" >&2
    exit 1
fi
for command in curl docker dockerd containerd ctr runc \
    qemu-x86_64-static dnsmasq nft /usr/local/bin/hamnd \
    /usr/local/libexec/hamn/guest-json; do
    command -v "$command" >/dev/null
done
for plugin in bridge host-local loopback portmap firewall tuning; do
    test -x "/usr/lib/cni/$plugin"
done
systemctl is-enabled --quiet hamnd.service
systemctl is-enabled --quiet binfmt-support.service
# This offline check cannot establish binfmt registration, Rosetta, networking,
# Docker/Compose/Buildx operation or reboot persistence; physical gates own those.
cloud-init clean --logs --machine-id
rm -f /etc/ssh/ssh_host_*
rm -f /var/lib/systemd/random-seed
apt-get clean
rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/* \
    /var/log/journal/* /tmp/* /var/tmp/*
find /var/log -type f -exec truncate -s 0 -- {} +
# Empty machine-id is regenerated at first boot; dbus uses the same identity.
: >/etc/machine-id
if [ -L /var/lib/dbus/machine-id ]; then
    test "$(readlink /var/lib/dbus/machine-id)" = /etc/machine-id
elif [ -f /var/lib/dbus/machine-id ]; then
    rm /var/lib/dbus/machine-id
    ln -s /etc/machine-id /var/lib/dbus/machine-id
fi
# Identical package files (runc in /usr/bin and /usr/sbin, git's commands,
# perl's two Unicode tables) share one inode. Mode, owner, timestamps and
# extended attributes must match, and dpkg replaces a linked file by rename.
hardlink --respect-xattrs /usr >/dev/null
