# Architecture

This is the canonical English architecture reference for Hamn 0.0.1. See
[ARCHITECTURE.ko.md](ARCHITECTURE.ko.md) for the Korean translation.

## Design boundary

Hamn is a macOS CLI that owns a profile-scoped Linux VM and the narrow host ↔
guest transports around it. It is not a container engine, a Docker CLI
replacement, a Desktop application, or a host containerd distribution.

macOS XNU does not implement the Linux kernel ABI required by Linux container
processes, including Linux namespaces, cgroups, and the overlay filesystem.
The product boundary is therefore a Linux guest VM, created with Apple
Virtualization.framework, rather than an attempt to run a Linux container
directly on macOS. Apple's guide documents the architecture-specific Linux
image and VM-device configuration this requires: [Creating and Running a Linux
Virtual Machine](https://developer.apple.com/documentation/virtualization/creating-and-running-a-linux-virtual-machine).

```text
macOS, profile <name>                         Ubuntu 24.04 arm64 guest
────────────────────────────────────────────  ─────────────────────────────────
hamn CLI                                       Linux kernel
  lifecycle lock                               hamnd (small guest-control agent)
  VZ VM owner                                  dockerd
  SSH ControlMaster                             system containerd + CRI plugin
  Docker socket forward                         runc, CNI, BuildKit, binfmt
  port observer / TCP forward / UDP relay       optional K3s + kubelet
  virtiofs mount setup
```

The guest is a release-selected, preconfigured image. It contains the runtime binaries,
`hamnd.service`, guest helper scripts, K3s compatibility manifest, and public
keys. On boot, Hamn only reconciles profile-controlled configuration, such as
Docker daemon JSON and Rosetta selection. It does not rsync source code into a
running VM or compile the guest runtime on first boot.

## Process and socket ownership

One profile owns one private state directory:

```text
~/.hamn/<profile>/
  config.yaml               strict profile configuration
  disk.img                  guest data disk
  docker.sock (0600)        forwarded guest Docker Engine API
  agent.sock                forwarded Hamn guest-agent control socket
  state.json, VM PID, locks, logs, SSH control path, port records
  kubeconfig                only when K3s is explicitly enabled
```

The default profile's Docker context is `hamn`; named profiles use
`hamn-<profile>`. The profile-local Docker socket is the only container API
Hamn exposes to the host. It forwards to `/var/run/docker.sock` inside the
guest through SSH. Host `/var/run/docker.sock` is never created, replaced, or
used by Hamn.

The system containerd socket, `/run/containerd/containerd.sock`, remains in the
guest. It is a native containerd endpoint, not a Docker Engine endpoint and not
a supported host API. `status --json` reports Docker API readiness separately
from CRI readiness instead of exposing this socket.

## Docker path

```text
macOS Docker CLI / Compose / buildx / SDK
  │  Docker Engine API
  ▼
~/.hamn/<profile>/docker.sock  (0600, SSH Unix-socket forward)
  ▼
guest /var/run/docker.sock
  ▼
guest dockerd --containerd=/run/containerd/containerd.sock
  │  native containerd API, namespace moby
  ▼
guest system containerd
  ▼
runc -> Linux kernel
```

Docker documents that `dockerd` may be pointed at a separately started
containerd with `--containerd`, and that its default containerd namespace is
`moby`; see [dockerd](https://docs.docker.com/reference/cli/dockerd/). Docker's
logical state is owned by Docker (`/var/lib/docker` and `moby`); containerd's
native store is owned by the guest system service.

Hamn does not proxy arbitrary Docker requests itself. Its internal Docker
observer is limited to published-port synchronization: it reads Docker events
and inspect data, then owns only the macOS forwarding resources it creates.

Registry credentials remain a host Docker-client concern. A host Docker CLI or
SDK resolves its configured credential helper and sends registry authorization
with the individual Docker API request through the profile socket. Hamn does
not run `docker login`, copy a credential helper, or create a guest
`/home/hamn/.docker` credential store. The default home virtiofs share is a
user-exposed filesystem, not a credential-isolation boundary; do not treat it
as one.

## Kubernetes path

```text
macOS kubectl
  │  HTTPS Kubernetes API, profile-local kubeconfig
  ▼
SSH loopback forward -> guest K3s API server
  ▼
guest kubelet
  │  CRI gRPC
  ▼
guest system containerd, namespace k8s.io
  ▼
runc -> Linux kernel
```

CRI is the kubelet-to-runtime protocol; it is not the Docker containerd API.
The [Kubernetes CRI documentation](https://kubernetes.io/docs/concepts/containers/cri/)
defines that boundary. Host `kubectl` never connects directly to CRI or to the
containerd socket.

K3s is disabled on a new profile. When the user runs `hamn kubernetes start`,
the guest verifies a signed compatibility manifest and checksums, installs the
fixed K3s artifact, configures it to use the existing system containerd, and
waits for node and CoreDNS readiness. K3s owns its `k8s.io` namespace and K3s
state directories. Docker's `moby` namespace remains distinct.

The profile-local kubeconfig gets context `hamn` for the default profile and
`hamn-<profile>` for other profiles. A collision with a foreign context fails;
Hamn does not overwrite it. `hamn kubectl` rejects `--kubeconfig` overrides,
so it cannot silently operate on another cluster.

containerd's own guidance distinguishes its native CLI/API from CRI and notes
that the CRI plugin is built into containerd: [getting
started](https://github.com/containerd/containerd/blob/main/docs/getting-started.md).

## Lifecycle and rollback

`start` serializes profile mutation, validates immutable image inputs, prepares
the disk, SSH keys, cloud-init seed, mounts, and VM state, then launches the VZ
VM owner. Shared NAT discovers its DHCP lease. Hamn has no non-shared network
attachment or alternate guest-address reporting path; the container runtime
always remains inside the guest VM.

After SSH comes up, Hamn runs configured provisioning stages in this order:

```text
system -> user -> guest configuration transaction -> after-boot -> ready
```

The guest transaction snapshots the runtime-related files and service states
before it updates managed configuration. A failed Docker/containerd/K3s helper
step restores that snapshot. The host only records the configuration
fingerprint after the transaction commits and Docker plus containerd readiness
checks succeed.

`stop` and `delete` close the SSH forwards, Docker observer, TCP listeners,
UDP relays, and VM process state that the profile owns. A soft `delete`
preserves the disk. `delete --data` requires an exact interactive `y` before it
removes the profile directory.

## Mount and network boundaries

`$HOME` is the default virtiofs share and may be disabled or made read-only.
Custom host paths are canonicalized before VM launch. They must be absolute,
owned directories without symlink traversal; writable custom paths must remain
under `$HOME`, while external paths default to read-only.

Every Hamn profile uses Virtualization.framework shared NAT. Published TCP
ports use SSH ControlMaster forwards; published UDP ports use a bounded host
relay. Forward creation and removal are transactionally reconciled. Network
attachment is not configurable per profile: there is no `network` YAML key or
network-selection CLI option.
`host.docker.internal` is served to guest Docker networks; `host.hamn.internal`
is a 0.0.1 compatibility alias and guest Docker configuration warns before its
next-release removal.

## Compatibility boundaries

The guest defaults to `binfmt` for amd64 Linux images. Rosetta is opt-in, using
the Virtualization framework's Linux Rosetta directory share when the host
supports it. Nested virtualization is opt-in. On macOS 15 or later, Hamn uses
Apple's [nested-virtualization capability check](https://developer.apple.com/documentation/virtualization/vzgenericplatformconfiguration/isnestedvirtualizationsupported)
before enabling it; Apple documents that capability for Macs with an M3 chip or
later.

There is no Intel Mac backend, Linux host backend, Incus runtime, GPU/AI
integration, external kubeconfig catalog, managed kind cluster, public
containerd socket, Desktop app, XPC service, Homebrew Cask, DMG, notarization,
or Docker shim in this release.
