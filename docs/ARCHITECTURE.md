# Architecture

Hamn is one macOS executable with a Rust control plane and a statically linked
C/Objective-C virtualization core. [API](API.md) describes the public requests.

## Shared control plane

```text
Ratatui/Crossterm TUI ─┐
                      ├─ typed Request → service → typed result / events
Clap headless CLI ────┘                       │
                         ┌───────────────────┼───────────────────┐
                         ▼                   ▼                   ▼
                    C worker             Bollard             kube-rs
                  same executable      Docker Engine     external Kubernetes
                         │               Unix socket          kubeconfig
                         ▼
                 profile VM lifecycle
                 Virtualization.framework
```

`control/` contains both frontends, operation validation, JSON envelopes, API
clients, bounded log streams, and cancellation. A TUI operation uses exactly
the same service as its headless equivalent. A view generation discards late
responses from a previously selected target. Network calls run asynchronously.

C functions in `host/core/control.h` execute only in a fresh `__core-worker`
process. The entrypoint dispatches internal C modes before starting Tokio or
initializing a terminal. This keeps C globals, fork, and exit outside Rust's
multithreaded process. The worker protocol is generated JSON, not legacy CLI
text. The caller frees C results with `hamn_control_free`.

## VM and socket ownership

C owns profile configuration, VM identity checks, lifecycle and mutation locks,
SSH ControlMaster, port observers, image validation, and profile state. The
Objective-C Virtualization.framework boundary stays inside `host/vz/`.

Each profile owns its disk, SSH key, `vmrun` identity, `vmrun.sock`, `ssh.sock`,
`docker.sock`, and `agent.sock` below `~/.hamn/<profile>/`. The long-lived VM
owner is another process of the same executable. Closing a TUI cancels that
frontend's operations without stopping the VM. C supervisors retain locks
and reap subprocesses when an operation worker disappears.

Docker API requests flow through the profile's SSH-forwarded Unix socket to
guest dockerd. Docker uses system containerd's `moby` namespace. The C port
observer continues to reconcile published TCP and UDP ports. External Docker
CLI, Compose, buildx, SDKs and Testcontainers use the same public socket;
Hamn does not switch their current context. Registry credentials remain the
external client's responsibility. The home share is not a credential-isolation
boundary.

Kubernetes uses a kube-rs client against the selected external context. It does
not use the Hamn VM, guest CRI, or a Hamn API forward. Kubeconfig merging and
credentials are read locally; context selection does not rewrite source files.
Mutations resolve object identity and use resource-version/UID preconditions.
Automatic HTTP retries are disabled to avoid replaying a mutation.

## Guest configuration and retirement

The signed Ubuntu 24.04 arm64 image owns hamnd, Docker, shared containerd, runc,
CNI, binfmt and normal guest helpers. There is no unsigned cloud-image fallback
or source-directory mount used to build guest code during VM startup.

For a legacy profile, SSH readiness starts managed K3s retirement before normal
provisioning. The existing EFI boot path is preserved; old K3s can briefly run
before SSH becomes available. The fixed Python payload and replacement verifier
and transaction helper are embedded in the signed host binary. This narrowly
scoped, one-time replacement updates existing guest disks without treating the
host checkout as a general guest configuration source.

The root-owned guest journal records verified ownership, service stop/mask,
`k8s.io` resource cleanup, dedicated file removal, helper replacement and Docker
readiness. Interrupted stages retry. Shared content, Docker objects, user
mounts and source kubeconfig files are preserved. K3s data deletion cannot be
undone by restoring an older binary. C publishes the new profile format only
after guest retirement and profile-local forward cleanup succeed.

The normal guest transaction snapshots managed runtime configuration and
service state before changing it. The deployment fingerprint is recorded after
commit and Docker/containerd readiness. Retirement is separate from that
rollback: restoring runtime configuration does not resurrect K3s data.

## Single executable build

`make host` builds a C/Objective-C static archive, links the Rust executable,
then signs and inspects a temporary candidate before atomically publishing
`build/hamn`. Cargo.lock and rust-toolchain.toml pin dependencies and compiler.
System macOS libraries, SSH, guest images/binaries and kubeconfig exec plugins
are permitted dependencies. No separate host core binary or dedicated shared
library is required at runtime.

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
integration, managed kind cluster, public
containerd socket, Desktop app, XPC service, Homebrew Cask, DMG, notarization,
or Docker shim in this release.
