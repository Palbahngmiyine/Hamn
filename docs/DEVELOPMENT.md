# Development

See [DEVELOPMENT.ko.md](DEVELOPMENT.ko.md) for Korean.

Hamn targets Apple Silicon macOS 13 or later. Rust owns the Ratatui TUI,
headless interface, and Docker/Kubernetes clients. C11 owns VM lifecycle,
profiles, images, SSH, and forwarding. Objective-C stays in `host/vz/`.
The GNU11 guest agent is part of the immutable Ubuntu image.

## Build

Install the macOS command-line developer tools and the Rust toolchain pinned
in `rust-toolchain.toml`, then run:

```sh
make host
build/hamn --version
build/hamn --headless capabilities
```

Cargo locks dependencies in `Cargo.lock` and builds C/Objective-C into a static
archive in its own output directory. `scripts/build-host.py` serializes
publication, signs and checks a temporary executable, then atomically replaces
`build/hamn`. The single Mach-O uses macOS system libraries and the existing
Virtualization entitlement. This is ad-hoc signing, not Developer ID signing
or notarization. Do not distribute a separate core executable or dynamic library.

Docker CLI is optional for users of the built-in Engine API client. External
Docker CLI, Compose, buildx, and SDKs can use the profile's public socket.
A VM needs an installed, verified managed guest image; there is no unsigned
cloud-image fallback. External Kubernetes operations do not need a Hamn VM.

## Source gates

```sh
make test-control
make test-profile-state
make test-guest-deployment
make test-release-gate
make test-local-macos
```

`test-control` covers Rust services, the C boundary, worker isolation, mock
Docker/Kubernetes APIs, K3s retirement, terminal restoration in a PTY, and
binary publication. `test-release-gate` tests the evidence contract without a
VM. Neither is physical release proof. `test-local-macos` runs the local
source, guest, installation, update, and release gates; it requires actionlint.
Run Make gates serially: packaging/update fixtures temporarily build other
versions into the public `build/hamn` path.

## Guest images and retirement

`guest/image/release-inputs.json` pins the Ubuntu base URL and SHA-256.
`guest/image/build-ubuntu-24.04-arm64.sh` runs on Linux arm64 with libguestfs.
Supply `HAMN_GUEST_BASE_IMAGE`, `HAMN_GUEST_BASE_SHA256`, and
`HAMN_GUEST_OUTPUT`; the builder verifies the base digest and archives only
committed `guest/` and `vendor/` sources. Docker, containerd, runc, CNI, binfmt,
DNS, and hamnd remain image-owned. New images contain no managed K3s.

The fixed payload in `host/migration/` is embedded in the signed host binary.
It may retire K3s and replace the old guest verifier/helpers once; this is not
a general host-to-guest software installation path. Retirement records its
steps, preserves Docker's `moby` namespace and shared content, and verifies
Docker readiness before completing. K3s data cannot be restored by rolling
back the host binary. See [Configuration](CONFIGURATION.md).

## Runtime validation

Use an isolated HOME, owned test profiles, and an explicitly selected test
Kubernetes context. Never run destructive tests against an existing user VM.
The external Kubernetes harness creates a unique namespace and removes it in
`finally`; it verifies kubeconfig bytes are unchanged:

```sh
python3 packaging/release/external-kubernetes-e2e.py --help
packaging/release/physical-e2e.sh --help
```

`make release-gate` runs the harness extracted from an exact candidate, with
prepared running/stopped legacy fixtures and a pinned legacy binary. It must
prove Docker data preservation on actual Apple Silicon. See
[Release setup](RELEASE-SETUP.md) for runner inputs and promotion authority.

## Source boundaries

- `control/`: typed requests/results, shared services, TUI, headless output.
- `host/core/`: C ABI, profiles, VM lifecycle, image and migration coordination.
- `host/vz/`: Virtualization.framework only.
- `host/fwd/`: owned Docker socket and published-port forwarding.
- `guest/agent/` and `guest/scripts/`: guest management and image-owned helpers.
- `packaging/release/`: exact candidate assembly, validation, promotion.

Internal worker dispatch runs before terminal or asynchronous runtime setup.
Keep C process-global state and fork/exit behavior in the worker. TUI exit
must not terminate the independently owned VM supervisor. When changing an
interface, update English and Korean references and success/failure tests.
