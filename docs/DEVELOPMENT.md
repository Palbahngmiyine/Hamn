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
make -j1 test-workspace-live WORKSPACE_LIVE_CACHE="$HOME/.hamn/cache"
```

Run it inside `nix develop .#live`, which provides Docker CLI with its
Compose/buildx plugins, kubectl, and kind. The cache must contain the selected signed guest image and verification marker. The harness
creates an owned `/tmp` HOME, uses only its explicit Docker socket, and records
binary/image hashes and results there. Its children, including the TUI under
test, find only the Docker CLI, kubectl, and kind it resolved from `PATH`, then
Homebrew and system directories. It checks backup/socket recovery, data
preservation, published TCP and UDP ports, cancellation/forced worker exit,
native PTY commands, and Kubernetes apply/exec/port-forward. It deletes the
kind cluster and stops its VMs; the test
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


### PR #65: validation boundaries and exact revision replay

PR #65's head is `58d76aae987ba834e026703c0681a771c3b33292`.
Actions run [37835838338](https://github.com/Palbahngmiyine/Hamn/actions/runs/37835838338)
passed its portable job and five macOS shards. The PR records `workspace-live`
on `9f4b501`, declarative VM checks on `dcf5838`, and unreadable running-profile
checks on `b4d13fd`. None establishes live success on `58d76aa`. The PR is now
merged; this follow-up does not alter its reported historical evidence.

| Path | Automated scope | Not proved |
|---|---|---|
| Portable job / `test-portable` | Repository and guest-script contracts | macOS host or booted guest |
| Shard 3 / `test-profile-state` | Real headless apply, semantic no-op, locks, fake supervisor identity, write/read-back/allocator failures, explicit corrupt/permission repair | Actual Virtualization.framework VM or guest settings |
| Shard 4 / `test-control-tui` / `workspace-live-checks` | Harness guards, oracles, PTY and transport fixtures | Real VM, Docker Engine or kind cluster |
| Opt-in `test-workspace-live` / `workspace-live` | Actual VM/guest, Docker, Compose/buildx, kind; includes live declarative checks | Success without an executed run and its evidence |

The new repair case is in the existing `vm-apply` suite and therefore shard 3;
no additional CI job is needed. Hosted CI continues to check harness compilation
and VM-free regressions. Keep physical runs separate: a dedicated Apple Silicon
host can run the opt-in Make target on a pinned checkout. A future manually
triggered workflow should use a dedicated runner label, trusted refs only,
serialized VM access, and always retain the log, source SHA/diff, binary SHA-256,
image SHA-256 and ownership root. Do not schedule untrusted PR code on a personal
self-hosted machine or mark a skipped physical job as physical success.

Requirements: physical Apple Silicon macOS 13+, an unprivileged account, Apple
command-line developer tools, Nix with flakes and the locked `.#live` shell
(pinned Rust, Docker CLI, Compose/buildx, kubectl, kind), a verified signed guest
image in the cache, Internet access for container images, and sufficient disk/RAM
for a 60 GiB sparse disk, a 6 GiB VM and kind. The harness verifies the selected
image digest and marker and copies the cache into its own HOME. No unsigned image
fallback is permitted. Do not run other gates concurrently in this checkout.

To replay the **unmodified historical head** in a fresh checkout:

```sh
git clone https://github.com/Palbahngmiyine/Hamn.git Hamn-pr65-live
cd Hamn-pr65-live
git checkout --detach 58d76aae987ba834e026703c0681a771c3b33292
nix develop .#live --command make -j1 host
set -o pipefail
nix develop .#live --command target/release/hamn-dev test workspace-live \
  --binary build/hamn --cache "$HOME/.hamn/cache" 2>&1 | tee workspace-live.log
```

This reproduces the old workspace suite; it does **not** contain the new apply
assertions. On the follow-up checkout, record the candidate and run:

```sh
git rev-parse HEAD > workspace-live-source.txt
git diff --binary > workspace-live-source.patch
set -o pipefail
nix develop .#live --command make -j1 test-profile-state test-control
nix develop .#live --command make -j1 test-workspace-live \
  WORKSPACE_LIVE_CACHE="$HOME/.hamn/cache" 2>&1 | tee workspace-live.log
shasum -a 256 build/hamn > workspace-live-binary.sha256
```

The new live stage repeats apply while running (no write, same PID), rejects a
changed definition (`conflict`, same file/PID and guest), applies CPU/RAM changes
while stopped, and checks CPU count, Linux MemTotal and a writable virtiofs share
inside the guest after restart. It also verifies that corrupt running-profile
status/stop/apply refuse without stopping Docker, and that malformed YAML or
permission denial never triggers automatic replacement. Repair explicitly restores
known-good bytes or mode 0600; reapply is then a no-op and the guest boots again.
Only the test-owned configuration is restored in cleanup; no disk is removed.

The printed ownership root contains `binary-sha256.txt`, `ownership.json`
(including guest image digest), and `vm-apply-results.json`. A stage result alone
is not success of the full suite: require exit 0, including VM/cluster cleanup,
and keep the log. Fresh candidates need fresh roots. Docker daemon JSON, Rosetta,
SSH agent and provision hooks are not exercised by this new apply stage; it does
not claim to validate every profile setting.

If a real profile is unreadable, retain its disk, keys, PID/identity and a copy of
its configuration. Restore a known-good configuration with private permissions,
then re-observe status before stop or apply. `vm apply` is not a force-repair or
force-stop command, and its omitted fields reset to defaults. `outcomeUnknown`
after a durability failure requires re-observation; a retry may be a no-op.

Validation of this follow-up in the Linux editing environment: shard partition
and `git diff --check` passed. Five portable scripts passed (containerd, Rosetta,
install targets, image contract, image builder). Docker fixture failed because
socket creation is prohibited; deployment transaction fixture refused root, and
an unprivileged retry was blocked by process permissions. Rust/Nix are unavailable.
New Rust regressions, macOS gates and real VM/kind checks were **not run** here.
