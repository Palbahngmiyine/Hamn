#!/bin/bash
# Runs offline inside the newly provisioned, disposable image only.
set -euo pipefail
export LC_ALL=C

# Protect the runtime dependency closure before purging build dependencies.
# gcc/make were installed only for hamnd; nothing compiles at normal VM startup.
# Hamn's guest tools are C and shell and need no interpreter; packages that
# the base image's own cloud-init depends on stay installed through apt.
apt-mark manual curl docker.io containerd runc \
    containernetworking-plugins qemu-user-static binfmt-support dnsmasq nftables
apt-get -y purge gcc make
apt-get -y autoremove --purge
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
