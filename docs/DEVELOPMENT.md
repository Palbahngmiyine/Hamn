# Development

See [DEVELOPMENT.ko.md](DEVELOPMENT.ko.md) for Korean.

Hamn targets Apple Silicon macOS 13 or later. Rust owns the Ratatui TUI,
headless interface, and Docker/Kubernetes clients. C11 owns VM lifecycle,
profiles, images, SSH, and forwarding. Objective-C stays in `host/vz/`.
The GNU11 guest agent is part of the immutable Ubuntu image.

## Test environment

Install Apple's command-line developer tools and [Nix](https://nixos.org/download/)
with flakes enabled once. The locked `flake.nix` provides everything else: the
Rust release in `rust-toolchain.toml` with rustfmt and clippy, jq,
actionlint, Git, GNU Make, OpenSSH, and Docker CLI with its Compose and buildx
plugins. No Homebrew, apt, or Rust installer step is needed.

```sh
nix develop                                          # interactive development shell
nix develop .#ci --command make -j1 test-local-macos # every local macOS gate
nix develop .#live                                   # also kubectl and kind
```

On macOS the shells use Apple's `/usr/bin` compiler, linker, and `codesign`,
export the system SDK as `SDKROOT` (never a Nix SDK), put no Nix C compiler on
`PATH` (the Rust toolchain does not propagate nixpkgs' clang wrapper), and place
the macOS userland (`stat`, `sed`, `find`, `tar`) ahead of stdenv's GNU tools
because Hamn's scripts target it. Pinned Nix tools stay first on `PATH`. CI runs the
same shells, and `make test-core-quality` checks this resolution inside them.

## Build

Inside `nix develop` (or with the Apple command-line developer tools and the
Rust toolchain pinned in `rust-toolchain.toml`), run:

```sh
make host
build/hamn --version
build/hamn --headless capabilities
```

Cargo locks dependencies in `Cargo.lock` and builds C/Objective-C into a static
archive in its own output directory. `hamn-dev build-host` (the `tools/hamn-dev`
workspace crate, never shipped) serializes publication, signs and checks a temporary executable, then atomically replaces
`build/hamn`. The single Mach-O uses macOS system libraries and the existing
Virtualization entitlement. This is ad-hoc signing, not Developer ID signing
or notarization. Do not distribute a separate core executable or dynamic library.

`make install` publishes `build/hamn` as a new managed generation with the
executable's own native installer (`build/hamn __install-support install`), under
`PREFIX` (default `~/.local`); `BINDIR` and `DATADIR` select the command and data
directories. It installs only the executable, with no source files, migrates a
verifiable Hamn 0.1.x installation and refuses any other earlier installation
unchanged; see
[Installation](INSTALLATION.md#installed-layout-and-earlier-installations).
Its migration tests install Hamn 0.1.2 with the installer from the v0.1.2
release commit, so the installer gates need a full clone, not a shallow one.

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
Docker/Kubernetes APIs, guest deployment recovery, terminal restoration in a PTY, and
binary publication. `test-release-gate` tests the evidence contract without a
VM. Neither is physical release proof. `test-local-macos` runs the local
source, guest, installation, update, and release gates; it requires actionlint,
which the Nix shells provide.
Run Make gates serially: packaging/update fixtures temporarily build other
versions into the public `build/hamn` path.

Pull-request CI splits these gates into `make ci-macos-shard-1` through
`ci-macos-shard-5` on separate runners. Within a shard, gates that never rebuild
`build/hamn` run beside the main lane, in a side lane (`CI_MACOS_SIDE_GATES`) and an
updater lane (`CI_MACOS_SHARD_<n>_UPDATER`), and gates that build other versions
(`CI_MACOS_ALONE_GATES`) run last. PR CI builds with
`CARGO_PROFILE=ci` (the release profile without LTO). `make check-ci-macos-shards`
fails unless the shards together run every `test-local-macos` gate exactly once.
The release workflow still runs `test-local-macos` serially with the release
profile, the one that builds published artifacts.

## Guest images

`guest/image/release-inputs.json` pins the Ubuntu 24.04 Minimal cloud image URL
and SHA-256.
`guest/image/build-ubuntu-24.04-arm64.sh` runs on Linux arm64 with libguestfs.
Supply `HAMN_GUEST_BASE_IMAGE`, `HAMN_GUEST_BASE_SHA256`, and
`HAMN_GUEST_OUTPUT`; the builder verifies the base digest and archives only
committed `guest/` and `vendor/` sources. Docker, containerd, runc, CNI, binfmt,
DNS, and hamnd remain image-owned; images contain no managed K3s. The host
never installs guest software; it only sends the fixed deployment recovery
script described in [Architecture](ARCHITECTURE.md).

## Runtime validation

Use an isolated HOME, owned test profiles, and an explicitly selected test
Kubernetes context. Never run destructive tests against an existing user VM.
The external Kubernetes harness creates a unique namespace and removes it even
when a check fails; it verifies kubeconfig bytes are unchanged:

```sh
make hamn-dev
target/release/hamn-dev release external-kubernetes-e2e --help
target/release/hamn-dev release physical-e2e --help
```

`make release-gate` builds `hamn-dev` from the checkout and runs the physical
harness against the executable archived in an exact candidate on actual Apple
Silicon. See [Release setup](RELEASE-SETUP.md) for runner inputs and promotion
authority.

## Source boundaries

- `control/`: typed requests/results, shared services, TUI, headless output.
- `control/install_support/`: the native installer and updater (`__install-support`).
- `host/core/`: C ABI, profiles, VM lifecycle and guest deployment transactions.
- `host/vz/`: Virtualization.framework only.
- `host/fwd/`: owned Docker socket and published-port forwarding.
- `guest/agent/` and `guest/scripts/`: guest management and image-owned helpers.
- `packaging/release/`: the release installer template.
- `tools/hamn-dev/` (`hamn-dev release`): exact candidate assembly, validation,
  promotion.

Internal worker dispatch runs before terminal or asynchronous runtime setup.
Keep C process-global state and fork/exit behavior in the worker. TUI exit
must not terminate the independently owned VM supervisor. When changing an
interface, update English and Korean references and success/failure tests.

## Workspace integration on Apple Silicon

Run deterministic TUI/guest checks with `make test-control` and
`make test-guest-deployment`, and the local release regression with
`make test-local-macos`. Keep HEAD fixed throughout the latter: artifact tests
bind the candidate to the source tree at their start.

For real VM, Docker, Compose, buildx, and disposable kind/Kubernetes validation:

```sh
make hamn-dev
target/release/hamn-dev test workspace-live --binary build/hamn --cache "$HOME/.hamn/cache"
```

Run it inside `nix develop .#live`, which provides Docker CLI with its
Compose/buildx plugins, kubectl, and kind. The cache must contain the selected signed guest image and verification marker. The harness
creates an owned `/tmp` HOME, uses only its explicit Docker socket, and records
binary/image hashes and results there. Its children, including the TUI under
test, find only the Docker CLI, kubectl, and kind it resolved from `PATH`, then
Homebrew and system directories. It checks backup/socket recovery, data
preservation, cancellation/forced worker exit, native PTY commands, and Kubernetes
apply/exec/port-forward. It deletes the kind cluster and stops its VMs; the test
HOME remains available for inspection. `--root` resumes only an owned test root;
`--keep-running` retains the main test VM for additional diagnosis. Remove that
owned test directory after reviewing evidence. Never use a user profile as a fixture.

`hamn-dev test workspace-live-management --root ROOT` reruns only the
management review against an existing kind cluster in a kept root's engine.
`hamn-dev test guest-image-live --help` lists the inputs of the same-contract
runtime test for locally built guest images, which gives each image its own
HOME and VM. `make test-control` runs `hamn-dev test workspace-live-checks`,
which checks the harness's guards, guest barrier scripts, and transport
fixtures without a VM.

The local control suite requires Docker CLI for the external-context transport
fixture. It connects only to the test-owned Unix socket; no Docker daemon or VM
is started. Every Nix shell includes `docker-client`.
