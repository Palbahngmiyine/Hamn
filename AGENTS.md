# Instructions

## Read First

- Before adding, modifying, or deleting code, read this file and follow it.
- Read the relevant source before writing tests or changing behavior.
- Treat the code as the source of truth. Do not assume behavior from names, docs, or prior memory alone.
- Check the current checkout, Git status, applicable directory instructions, and affected callers before editing. Preserve user-owned changes.
- Keep this file aligned with source and build targets. Describe proposed architecture as a proposal until implemented.
- Keep shared rules here and put specialized instructions near the code only when needed. Read applicable directory guidance before editing there; do not load unrelated subtrees.

## Three Principles

### 1. Clarity is the highest value

- Make behavior, data flow, ownership, and failure handling understandable from the local code and its explicit contracts.
- Choose the simplest design that preserves correctness, safety, and compatibility. Fewer lines or files are not improvements when they hide those obligations.
- Start with clear data structures and state transitions, then direct algorithms. Add abstraction only when it removes real duplication or protects a required boundary.
- Measure before optimizing. Record a representative workload and baseline; accept added complexity only for a demonstrated improvement without correctness regressions.
- Use names for meaning and units, and comments for invariants and reasons. Keep tests as readable and reviewable as production code.

### 2. Rigor means evidence of correct behavior

- Treat tests as a first-class investment. Derive expected results from the contract, independently of the implementation under test.
- Test the ways a change can fail, including failures during recovery. Passing builds, test counts, and coverage percentages alone do not establish correctness.
- Use SQLite as a standard of care, not a prescribed methodology: choose checks for Hamn's actual failure modes and explain verification gaps.
- Preserve acceptance criteria while repairing an implementation. Never weaken checks or remove defensive code merely to obtain a passing result.

### 3. Modularize for recursive improvement

- A module owns a design decision that can change behind a clear contract. Split by responsibility, hidden representation, and independently testable change; do not split mechanically by file length or execution step.
- Keep tightly coupled state and invariants together. Start with private Rust modules/functions or narrow C headers; add traits, function tables, or crates only when a concrete boundary requires them.
- Keep dependencies explicit and avoid cycles. Callers use the module contract rather than reaching into its state. Do not impose a universal untyped module interface.
- Separate policy and state-transition decisions from OS, network, clock, and process effects where this enables independent testing. Provide narrow seams for controlled events and dependency failures.
- Record each changed module's contract beside its public declarations or in existing architecture/API documentation: valid inputs and units, size limits, outputs and errors, ownership and lifetime, side effects, deadlines and cancellation, concurrency, and compatibility.
- Recursively narrow a problem to the smallest meaningful change, then verify upward through every affected caller and shared resource. Modules form a dependency graph; passing one child's tests is not proof that the combined system works.
- Improve implementations through normal source changes, rebuilds, and validation. Preserve the single executable; this principle does not require runtime code replacement or an autonomous improvement framework.

## Repository Map

- `control/`: Rust TUI, headless requests, shared service, Docker/Kubernetes clients, streams, and cancellation. `control/main.rs` is the executable entrypoint.
- `host/`: C/Objective-C VM core, statically linked into the Rust executable through `build.rs` and `Makefile`.
- `host/vz/`: the only place for Objective-C Virtualization.framework code.
- `host/core/`: profile configuration/state, lifecycle, control API, provisioning, and legacy K3s retirement coordination.
- `host/fwd/`, `host/sshmgr/`, `host/vmrun/`: profile-local forwarding,
  SSH control, and VM ownership. Keep resource ownership explicit and atomic.
- `host/seed/`, `host/util/`: cloud-init seed generation and shared low-level helpers, respectively.
- `host/image/`: signed managed guest-image selection and verification. Never add an
  unsigned cloud-image fallback.
- `host/migration/`: embedded, narrowly scoped legacy K3s retirement payload and service reference; not an active managed Kubernetes runtime.
- `guest/agent/`: Linux guest management agent `hamnd`; it is not a container engine.
- `guest/scripts/`: guest configuration for system containerd, Docker, Rosetta, and
  immutable-image validation.
- `guest/image/`: external Linux builder for the signed Ubuntu 24.04 arm64 guest image.
- `guest/systemd/`: the `hamnd.service` unit.
- `packaging/release/`: candidate assembly, physical validation evidence, and stable
  promotion. It must not rebuild an already validated RC.
- `tests/host/`, `guest/tests/`, and inline Rust tests: host, guest, and control-plane regressions. `tests/release/` and `packaging/release/` cover release evidence and runtime validation.
- `docs/`: English source documentation and corresponding Korean Markdown translations.
- `vendor/`: vendored C dependencies. Avoid replacing these casually.

Tracked source deliberately excludes Desktop/XPC/Cask/DMG/notarization, the legacy
`hamn-engine`, a public containerd socket, `nerdctl`, and any `docker -> hamn` shim.
An untracked `desktop/` directory can be user-owned; never modify or remove it while
working on the CLI-only product.

## Runtime Boundaries

- TUI and headless operations share `control/service.rs`; keep validation, mutation semantics, and error meanings consistent across both frontends.
- C control calls run in a fresh, single-threaded `__core-worker` process of the same executable. Preserve this boundary around C globals, fork, and exit; do not move blocking C work into the TUI's async executor.
- At the foreign-function interface (FFI), keep unsafe calls narrow and document pointer validity, length, lifetime, ownership, and error conversion. Release C control results with `hamn_control_free`; do not expose Rust internal layouts or serialize raw struct memory as a protocol.
- Docker uses each profile's forwarded Unix socket. Preserve external Docker CLI, Compose, buildx, SDK, and Testcontainers compatibility and explicit target selection.
- Kubernetes uses external kubeconfig contexts through `control/kubeconfig.rs` and `control/kubernetes.rs`. Preserve source kubeconfig files and mutation identity preconditions; do not restore guest K3s management or guest CRI as a public API.
- Keep host, guest, and vendored code ownership separate. Normal VM startup must not install mutable guest sources from the host checkout.

## Build Commands

Host builds require Apple Silicon macOS 13+, Apple command-line developer tools,
the Rust toolchain in `rust-toolchain.toml`, and Python 3. See
[Development](docs/DEVELOPMENT.md) for setup; `scripts/ci/setup-test-dependencies.sh`
lists test dependencies. Workflow checks require `actionlint`.

Run Make test gates serially in the same checkout, for example
`make -j1 test-local-macos`. Packaging/update fixtures replace the shared
`build/hamn` with other versions. Do not run separate gates concurrently unless
their build outputs and fixtures are verified to be isolated.

- `make host`: build, ad-hoc codesign, verify, and publish the single Rust + C/Objective-C `build/hamn`. Use `Cargo.lock` and `rust-toolchain.toml`.
- `make install`: install only `hamn` and its versioned source into `~/.local`.
- `make test-control`: Rust unit tests plus worker, Docker/Kubernetes API, TUI, SSH deadline, and single-binary regressions.
- `make test-workflows`: lint GitHub Actions workflows with `actionlint`.
- `make test-portable`: portable source and guest-script checks; this does not build or run the full guest agent.
- `make test-qcow2 HAMN_QCOW2_IMAGE=/path/to/guest.img`: verify a signed guest-image
  fixture without downloading a cloud image.
- `make test-profile-state`: verify profile/state persistence without starting a VM.
- `make test-guest-deployment`: verify the immutable guest contract and guest scripts.
- `make -C guest test-agent`: build and test the guest agent in a Linux environment.
- `make test-local-macos`: run static, portable, host, profile, deployment, release,
  and workflow gates that do not need a physical VM.
- `make release-candidate` and `make release-gate`: assemble a candidate and run the
  physical Apple Silicon validation harness. The second command is not a substitute for
  the required physical environment and exact RC artifacts.
- `make release-hosted-validation`: run the hosted validation path; preserve its evidence requirements and distinguish it from physical validation.
- `make clean` and `make -C guest clean`: remove host or guest build outputs.

`test-local-macos` does not include `make -C guest test-agent`; run the Linux
agent tests separately when changing agent behavior. Release inputs and exact
candidate requirements are in [Release setup](docs/RELEASE-SETUP.md).

After `make install`, `hamn` opens the TUI in a terminal. Use
`hamn --headless vm status` and `hamn --headless vm start --yes` for automation;
headless mutations require `--yes`. Consult `control/model.rs` and `docs/API.md`
for current operations. Hamn's structured Docker operations coexist with the
separately installed Docker CLI/API; they do not implement arbitrary Docker CLI
passthrough. Docker uses guest containerd's `moby` namespace. The guest containerd
socket is never a host public API; there is no `hamn nerdctl` or `--runtime` mode.

## Coding Rules

- Host C uses C11; guest code uses GNU11. Preserve `-Wall -Wextra` and `-Werror=implicit-function-declaration`.
- Use four-space indentation and `snake_case` function/variable names; keep C file-local helpers `static`.
- Follow existing Rust conventions and the pinned formatter. Keep `unsafe` obligations explicit; represent expected failures through the existing error contract.
- Assert important preconditions, postconditions, and expected-unreachable states. Required invariant checks must remain active in production.
- Make violations loud. Do not silently ignore impossible states, corrupt data, or partial failures.

## Testing Rigor

Apply this section and the fault/coverage checks below to changed behavior and
affected dependencies. Select checks from the contract and failure modes; a
local change does not require unrelated fuzzing or optimization experiments.

- Test success, failure, and meaningful decision branches, including relevant empty, zero, min/max, truncated, and malformed inputs. Explain untested paths.
- Add a regression for every bug fix that fails on the original defect and passes after the fix. Keep the smallest reproducer and verify its externally observable result.
- Validate state consistency, side effects, idempotency, rollback behavior, and resource cleanup.
- Make regressions reproducible: control clocks, input, failure points, and event order. Use explicit synchronization and bounded deadlines for process/PTY tests instead of arbitrary sleeps; record fuzz seeds and failing inputs.
- Name tests by behavior, edge case, or failure mode. Host shell tests use `tests/host/test_<feature>.sh`; guest tests stay in `guest/tests/`. Use `set -euo pipefail` in Bash tests.
- Exercise real behavior across module boundaries. Source-text checks and mocks supplement, rather than establish, runtime correctness. Use a contract-derived fixture or a separate observation path for critical state transitions.
- Run focused checks and affected integration checks. Use `test-control` for Rust/worker/API/TUI changes, add `test-profile-state` for profile/lifecycle changes, and `test-guest-deployment` for guest configuration changes. Use the Linux agent test command above for agent behavior.
- Documentation-only edits require source/command/link consistency checks and `git diff --check`. Do not run a VM solely for a prose edit; do not make new runtime claims without runtime evidence.
- After required checks pass, review the final diff for scope, regressions, and user-visible behavior. Repeat or broaden checks only for new changes, failures, or unresolved risks. Report unavailable checks and their impact; do not label unverified acceptance criteria complete.

## Coverage, Faults, and Fuzzing

- Use coverage to find untested behavior; report measured scope and exclusions. Do not impose SQLite's 100% coverage targets or MC/DC (independent-condition coverage) on every module.
- For changed core decisions, exercise each meaningful branch and boundary and explain any remaining gap. For compound conditions, check which condition changes the result.
- Check test sensitivity: removing a guard or breaking a transition must fail an appropriate test. Use mutation tools when useful, not as a universal gate.
- Simulate dependency failures, including first-call, Nth-call, continuous failure, timeout, cancellation, and partial-success-then-failure cases.
- For stateful changes, inject relevant allocation, write, sync, rename, spawn, and remote-operation failures, including another failure during recovery. Inspect persisted state and leaked resources after reopening or restarting.
- Fuzz parsers and decoders such as qcow2, HTTP, Docker API, and registry inputs. Preserve every crash or fuzz finding as a permanent regression case.
- Run memory/undefined-behavior checks for affected C and FFI paths where supported; document platform limits rather than treating skipped checks as passes.
- Where possible, run optimized and unoptimized builds and compare observable output.

## Concurrency and Persistence

- Verify no races, deadlocks, duplicate execution, or ordering violations.
- For VM state, image store, socket, and filesystem changes, ensure atomic behavior, safe retry, recovery after interruption, and no corrupt intermediate state.
- Clean up all temporary files, sockets, child processes, SSH forwards, and VM resources created by tests or commands.
- Bound blocking SSH/process operations at the layer that can terminate and reap them. An outer async timeout alone is insufficient. Preserve independently owned VM supervisors when frontend operations end.
- A timed-out mutation can have taken effect remotely. Preserve `outcomeUnknown`, re-observe state before retrying, and avoid replaying non-idempotent actions blindly.
- Validate live behavior with an isolated disposable profile and known resource ownership. Never terminate or delete a resource based only on a reused PID or a matching name.
- Distinguish binary rollback from state recovery. Verify interrupted migrations and compatibility; legacy K3s data retirement is irreversible and must preserve Docker/user data.

## Improvement Workflow

For behavior or module-boundary changes, scale these steps to the task; a small
change needs a short explanation, not a separate planning document.

1. Identify the contract, goal, affected callers, baseline revision, and acceptance checks in existing comments, tests, or review notes.
2. Reproduce the defect or measure the baseline. Narrow to an independently testable change; keep shared-state and lock changes coherent.
3. Change the implementation and add regressions. Replacement implementations share contract tests; compare old/new output only when semantics should remain equivalent.
4. Verify the child, affected parent integrations, and original goal on the combined candidate. An SSH deadline fix must also verify lifecycle completion and TUI responsiveness.
5. Report the revision/diff, commands, environment, results, and limitations. Bind release/performance evidence to the tested artifact hash; a child's pass or an agent's report alone is insufficient.

If an implementation fails its contract, fix the implementation. If the contract
must change, first revise the affected scope and acceptance criteria within the
user's authorization; make compatibility and migration consequences explicit.
Keep evaluation changes visible and justified independently of the candidate;
do not let a candidate relax its own acceptance rules.

Keep recursive work bounded by the original goal and shared task limits. Repeated
identical failures call for new evidence or replanning, not indefinite splitting
and retries. Do not claim improvement when it has not been demonstrated.

## Release and Review Gate

- Before a release candidate, produce a concrete human-review checklist covering user-visible behavior, security posture, performance, compatibility, and rollback readiness.
- Re-run the full regression suite before every release candidate.
- Commit subjects should follow the existing style, such as `feat: ...`, `docs: ...`, or `chore: ...`, with milestone context when useful.
- Pull requests must list commands run, describe host vs. guest impact, and include relevant CLI output for VM lifecycle or Docker API behavior changes.

## Security and Configuration

- Do not commit cloud images, VM state, private SSH material, or files from `~/.hamn/`.
- Keep `host/entitlements.plist` changes minimal and explain any new entitlement.
- Treat Docker API, registry, archive, filesystem, and network inputs as untrusted.

## Source References

- Check current structure against `control/main.rs`, `control/service.rs`, `control/core.rs`, `host/core/control.h`, `build.rs`, `Makefile`, and `guest/Makefile`; keep [architecture](docs/ARCHITECTURE.md) and [API documentation](docs/API.md) consistent with those sources.
