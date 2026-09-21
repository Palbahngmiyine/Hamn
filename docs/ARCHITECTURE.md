# Architecture

Hamn is one macOS executable with a Rust control plane and a statically linked
C/Objective-C virtualization core. [API](API.md) describes the public requests.

## Shared control plane

```text
TUI VM / structured requests ─┐
                             ├─ Request → service → C worker / Bollard / kube-rs
Headless operations ─────────┘
TUI Docker / Kubernetes lists ── native CLI → bounded JSON query → tables
TUI native commands ─────────── owned PTY sessions → Docker / kubectl / plugins
Docker --context (headless) ──── Bollard → private socket → Docker CLI transport
```

`control/` contains both frontends, operation validation, JSON envelopes, API
clients, bounded log streams, and cancellation. VM and structured requests share the service. TUI native resource lists and
commands use installed Docker/kubectl: lists have bounded query lifetimes, while
interactive commands run in separately owned PTY sessions. Kubernetes selected
mutations still enforce UID/resourceVersion through the guarded kubectl path. A view generation discards late
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
observer validates published IPv4 TCP/UDP Ports from one container-list request
and commits only complete snapshots, without a per-container inspect loop. External Docker
CLI, Compose, buildx, SDKs and Testcontainers use the same public socket;
Hamn does not switch their current context. Registry credentials remain the
external client's responsibility. The home share is not a credential-isolation
boundary.

Headless Kubernetes uses kube-rs; native TUI browsing uses kubectl against the
selected external context. It does
not use the Hamn VM, guest CRI, or a Hamn API forward. Kubeconfig merging and
credentials are read locally; context selection does not rewrite source files.
Mutations resolve object identity and use resource-version/UID preconditions.
Automatic HTTP retries are disabled to avoid replaying a mutation.

## Guest configuration and retirement

The signed Ubuntu 24.04 arm64 image owns hamnd, Docker, shared containerd, runc,
CNI, binfmt and normal guest helpers. There is no unsigned cloud-image fallback
or source-directory mount used to build guest code during VM startup.

New profile disks use `host/image/raw_cache.c`. A digest-keyed cache bundle
contains the sparse raw base and an extractor-version, virtual-size and SHA-256
marker. A per-digest lock bounds waiting to 60 seconds; private staging, file
and directory fsync, and atomic publication prevent incomplete bases from
becoming visible. Reuse validates ownership, modes, link count, size and both
image/raw hashes. Interrupted staging is recovered under the same lock.
This cache performs no network requests.

APFS provisioning uses `fclonefileat`, the descriptor-based `clonefile(2)`
operation, then grows the private disk to the requested size. Only `EXDEV`,
`ENOTSUP` and `EOPNOTSUPP` permit direct sparse extraction from the verified
image descriptor. Permission, integrity and I/O errors fail closed. An existing
profile disk is never rebased or replaced; only an explicit larger configured
size grows its existing inode. Full hash validation adds first-use and reuse
I/O, so shared storage does not imply universally faster profile creation.

For a legacy profile, SSH readiness starts managed K3s retirement before normal
provisioning. The existing EFI boot path is preserved; old K3s can briefly run
before SSH becomes available. The fixed guest-side Python payload and replacement verifier
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

## Release installation support

`control/install_support/` implements the private `__install-support` mode in the
same executable, before Tokio or terminal initialization. It owns archive and
manifest validation, stable version decisions, manifest-only checks, artifact
acquisition and byte accounting, version-1 receipt compatibility, host install
locks, recovery references and obsolete-generation collection. Shell scripts retain
transaction ordering and transfer verified paths/identities as explicit arguments.
Before the host executable is authenticated, the bootstrap uses macOS's stock
`zsh/system` for the shared per-digest lock and bounded partial writes. It verifies
the pinned size and SHA-256 before reading the executable through `tar` stdout.
That process releases its download lock before the native updater runs. Subsequent
manifest, receipt and transfer decisions execute inside Hamn; host installation
does not run Python. Release-build validators and test references may use Python,
and legacy retirement's embedded Python payload runs in the guest.
The scanner has a process-group deadline, and both install roots remain locked
through update recovery and collection. See [Installation](INSTALLATION.md) for
retention and compatibility limits.

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
