# Instructions

## Read First

- Before adding, modifying, or deleting code, read this file and follow it.
- Read the relevant source before writing tests or changing behavior.
- Treat the code as the source of truth. Do not assume behavior from names, docs, or prior memory alone.

## Repository Map

- `host/`: macOS host runtime built with clang into `build/hamn`.
- `host/vz/`: the only place for Objective-C Virtualization.framework code.
- `host/core/`: Docker-only profile, lifecycle, configuration, update, and Kubernetes helpers.
- `host/fwd/`, `host/sshmgr/`, `host/vmrun/`: profile-local Docker/Kubernetes forwarding,
  SSH control, and VM ownership. Keep resource ownership explicit and atomic.
- `host/image/`: signed managed guest-image selection and verification. Never add an
  unsigned cloud-image fallback.
- `guest/agent/`: Linux guest management agent `hamnd`; it is not a container engine.
- `guest/scripts/`: guest configuration for system containerd, Docker, K3s, DNS, and
  immutable-image validation.
- `guest/image/`: external Linux builder for the signed Ubuntu 24.04 arm64 guest image.
- `guest/systemd/`: units for `hamnd`, Docker integration, DNS, and K3s only.
- `packaging/release/`: candidate assembly, physical validation evidence, and stable
  promotion. It must not rebuild an already validated RC.
- `tests/host/` and `guest/tests/`: deterministic host and guest regression tests.
- `docs/`: English source documentation and corresponding Korean Markdown translations.
- `vendor/`: vendored C dependencies. Avoid replacing these casually.

Tracked source deliberately excludes Desktop/XPC/Cask/DMG/notarization, the legacy
`hamn-engine`, a public containerd socket, `nerdctl`, and any `docker -> hamn` shim.
An untracked `desktop/` directory can be user-owned; never modify or remove it while
working on the CLI-only product.

## Simplicity

- Simplicity is the highest implementation value.
- Apply KISS to implementation: choose the simplest design that satisfies all requirements and invariants.
- Prefer clear, direct C over clever abstractions. Add abstraction only when it removes real duplication or protects a required boundary.
- Keep host, guest, and vendored code ownership separate. Objective-C stays in `host/vz/` unless the architecture explicitly changes.

## Build Commands

- `make host`: build and ad-hoc codesign `build/hamn`.
- `make install`: install only `hamn` and its versioned source into `~/.local`.
- `make test-qcow2 HAMN_QCOW2_IMAGE=/path/to/guest.img`: verify a signed guest-image
  fixture without downloading a cloud image.
- `make test-profile-state`: verify profile/state persistence without starting a VM.
- `make test-guest-deployment`: verify the immutable guest contract and guest scripts.
- `make test-local-macos`: run static, portable, host, profile, deployment, release,
  and workflow gates that do not need a physical VM.
- `make release-candidate` and `make release-gate`: assemble a candidate and run the
  physical Apple Silicon validation harness. The second command is not a substitute for
  the required physical environment and exact RC artifacts.
- `make clean` and `make -C guest clean`: remove host or guest build outputs.

After `make install`, use `hamn start`, `hamn status`, and the separately installed
Docker CLI/API (`docker`, Compose, and buildx). Docker uses guest containerd's `moby`
namespace; opt-in K3s uses the same system containerd through CRI in `k8s.io`. The
guest containerd socket is never a host public API. There is no `hamn docker`,
`hamn nerdctl`, or `--runtime` mode.

## Coding Rules

- Host C uses C11; guest code uses GNU11. Preserve `-Wall -Wextra` and `-Werror=implicit-function-declaration`.
- Use four-space indentation, `snake_case` identifiers, and `static` for file-local helpers.
- Assert important preconditions, postconditions, and expected-unreachable states. Required invariant checks must remain active in production.
- Make violations loud. Do not silently ignore impossible states, corrupt data, or partial failures.

## Testing Rigor

- KISS does not apply to testing. Tests are a first-class investment.
- Test success and failure paths, all condition branches, and boundary values such as empty, zero, min, max, truncated, and malformed inputs.
- Add regression tests for every bug fix.
- Validate state consistency, side effects, idempotency, rollback behavior, and resource cleanup.
- Keep tests deterministic: no sleep-based timing, uncontrolled randomness, or unbounded retries.
- Name tests by behavior, edge case, or failure mode. Shell tests should be `tests/host/test_<feature>.sh` and use `set -euo pipefail`.

## Coverage, Faults, and Fuzzing

- Target 100% branch coverage on core logic.
- Apply MC/DC to boolean-heavy code.
- Use mutation-style reasoning: every branch should change observable behavior or state.
- Simulate dependency failures, including first-call, Nth-call, continuous failure, timeout, cancellation, and partial-success-then-failure cases.
- Fuzz parsers and decoders such as qcow2, HTTP, Docker API, and registry inputs. Preserve every crash or fuzz finding as a permanent regression case.
- Where possible, run optimized and unoptimized builds and compare observable output.

## Concurrency and Persistence

- Verify no races, deadlocks, duplicate execution, or ordering violations.
- For VM state, image store, socket, and filesystem changes, ensure atomic behavior, safe retry, recovery after interruption, and no corrupt intermediate state.
- Clean up all temporary files, sockets, child processes, SSH forwards, and VM resources created by tests or commands.

## Release and Review Gate

- Before a release candidate, produce a concrete human-review checklist covering user-visible behavior, security posture, performance, compatibility, and rollback readiness.
- Re-run the full regression suite before every release candidate.
- Commit subjects should follow the existing style, such as `feat: ...`, `docs: ...`, or `chore: ...`, with milestone context when useful.
- Pull requests must list commands run, describe host vs. guest impact, and include relevant CLI output for VM lifecycle or Docker API behavior changes.

## Security and Configuration

- Do not commit cloud images, VM state, private SSH material, or files from `~/.hamn/`.
- Keep `host/entitlements.plist` changes minimal and explain any new entitlement.
- Treat Docker API, registry, archive, filesystem, and network inputs as untrusted.
