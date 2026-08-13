# Development

This is the canonical English development guide. See
[DEVELOPMENT.ko.md](DEVELOPMENT.ko.md) for Korean.

Hamn 0.0.1 is a Docker-only local-container product for Apple Silicon macOS
13 or later. Host code is C11 and uses Virtualization.framework; Objective-C
code stays in `host/vz/`. Guest code is GNU11 and is built into the immutable
Ubuntu guest image, not copied from a host checkout into a running VM.

There is no Desktop, XPC, Docker shim, `hamn docker`, `nerdctl`, public
containerd socket, external Kubernetes catalog, Homebrew Cask, or DMG build
path to develop.

## Build

On an Apple Silicon Mac with the macOS command-line developer tools:

```sh
make host
build/hamn version
```

`make host` compiles `build/hamn` with C11 warnings and ad-hoc signs the
binary with only the Virtualization entitlement. It is not a Developer ID or
notarized distribution build.

Use a real Docker CLI separately; Hamn neither installs nor replaces it. A
local source binary has no selected guest image, so `hamn start` deliberately
fails until a signed release installs or updates one.

## Source gates

Run Make targets one at a time: several targets rebuild the shared
`build/hamn` path.

```sh
make test-portable
make test-core-quality
make test-profile-state
make test-guest-deployment
make test-diagnostics
make test-install
make test-uninstall
make test-update
make test-kubernetes-cli
make test-release-artifacts
make test-release-gate
make test-release-publish
```

`make test-local-macos` runs the applicable host/source gates serially and
also requires `actionlint`.

Do not run source tests concurrently when they rebuild the shared host binary.

## Guest-image work

The release guest image is Ubuntu 24.04 arm64. It must already contain
`hamnd`, Moby `dockerd`, system containerd and CRI, runc, BuildKit, CNI,
binfmt, guest configuration helpers, and the K3s manifest/trust material. The
image builder requires a trusted Linux arm64 environment and signed input
metadata:

```sh
bash guest/image/build-ubuntu-24.04-arm64.sh --help
```

The builder rejects absent signatures and checksums. It must run from a Git
checkout and archives only the committed `guest/` and `vendor/` trees while
preparing the image; untracked checkout files are never image inputs. It then
removes those sources from the finished root image. At VM boot, host code
supplies profile configuration and permitted virtiofs mounts only; it does not
mount `/opt/hamn` from the checkout or compile guest code.

## Local runtime discipline

Use an isolated `HOME` for destructive or lifecycle tests. Do not point a
source checkout at an existing `~/.hamn` directory, Docker context, or Colima
profile.

When changing an interface, update both canonical English and Korean Markdown
references, add a deterministic success and failure test, and run the smallest
relevant gate before broader gates. Keep host, guest, and vendored ownership
separate.

## Useful boundaries to inspect

- `host/cmd/` owns CLI parsing and profile lifecycle.
- `host/vz/` is the only Virtualization.framework implementation.
- `host/fwd/` owns Docker-published TCP/UDP forwarding, not a Docker API
  client.
- `guest/agent/` is the guest control agent; it is not a container engine.
- `guest/scripts/` configures image-provided Docker, containerd, K3s, and
  Rosetta components transactionally.
- `packaging/release/` builds, validates, and promotes release bytes.

For architectural boundaries, read [Architecture](ARCHITECTURE.md); for the
complete profile schema, read [Configuration](CONFIGURATION.md).
