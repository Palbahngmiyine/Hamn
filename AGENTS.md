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
- Keep each changed module's contract beside its public declarations: valid inputs and units, size limits, outputs and errors, ownership and lifetime, side effects, deadlines and cancellation, concurrency, and compatibility.
- Recursively narrow a problem to the smallest meaningful change, then verify upward through every affected caller and shared resource. Modules form a dependency graph; passing one child's tests is not proof that the combined system works.
- Improve implementations through normal source changes, rebuilds, and validation. Preserve the single executable; this principle does not require runtime code replacement or an autonomous improvement framework.

## Testing Rigor

Apply this section and the fault/coverage checks below to changed behavior and
affected dependencies. Select checks from the contract and failure modes; a
local change does not require unrelated fuzzing or optimization experiments.

- Test success, failure, and meaningful decision branches, including relevant empty, zero, min/max, truncated, and malformed inputs. Explain untested paths.
- Add a regression for every bug fix. Show that it fails on the unfixed code and passes after the fix. Keep the smallest reproducer and verify its externally observable result.
- Validate state consistency, side effects, idempotency, rollback behavior, and resource cleanup.
- Make regressions reproducible: control clocks, input, failure points, and event order. Use explicit synchronization and bounded deadlines for process/PTY tests instead of arbitrary sleeps; record fuzz seeds and failing inputs.
- Fix a timing flake at its cause. Judge the result by observable events, not by added sleeps, retries, or longer deadlines.
- Give every test its own HOME, sockets, install roots, and temporary files, and remove them on failure as well as success. Run checks that share build outputs or fixtures serially unless their isolation is verified.
- Name tests by behavior, edge case, or failure mode. Keep host and guest tests with their own side's suites. Use `set -euo pipefail` in Bash tests.
- Exercise real behavior across module boundaries. Source-text checks and mocks supplement, rather than establish, runtime correctness. Use a contract-derived fixture or a separate observation path for critical state transitions.
- Run the focused check of the changed module and the integration checks of every affected caller.
- Hosted CI does not replace physical Apple Silicon VM evidence. State which environment produced each result, and never infer physical VM, Docker, or Kubernetes behavior from a hosted run.
- Documentation-only edits require source/command/link consistency checks and `git diff --check`. Do not run a VM solely for a prose edit; do not make new runtime claims without runtime evidence.
- After required checks pass, review the final diff for scope, regressions, and user-visible behavior. Repeat or broaden checks only for new changes, failures, or unresolved risks. Report unavailable checks and their impact; do not label unverified acceptance criteria complete.

## Coverage, Faults, and Fuzzing

- Use coverage to find untested behavior; report measured scope and exclusions. Do not impose SQLite's 100% coverage targets or MC/DC (independent-condition coverage) on every module.
- For changed core decisions, exercise each meaningful branch and boundary and explain any remaining gap. For compound conditions, check which condition changes the result.
- Check test sensitivity: removing a guard or breaking a transition must fail an appropriate test. Do this through test-side seams or mutation tools, not by committing a disabled product guard. Use mutation tools when useful, not as a universal gate.
- Simulate dependency failures, including first-call, Nth-call, continuous failure, timeout, cancellation, and partial-success-then-failure cases.
- For stateful changes, inject relevant allocation, write, sync, rename, spawn, and remote-operation failures, including another failure during recovery. Inspect persisted state and leaked resources after reopening or restarting.
- Fuzz parsers and decoders such as qcow2, HTTP, Docker API, and registry inputs. Preserve every crash or fuzz finding, minimized and with its seed, as a permanent regression case.
- Run memory/undefined-behavior checks for affected C and FFI paths where supported; document platform limits rather than treating skipped checks as passes.
- Where possible, run optimized and unoptimized builds and compare observable output.

## Security and Configuration

- Do not commit cloud images, VM state, private SSH material, or files from the user's Hamn state directory.
- Tracked files hold no token-shaped credentials. Private keys appear only in test or vendored fixtures.
- Logs, errors, and diagnostics never print registry or kubeconfig credentials or SSH material.
- Keep host entitlement changes minimal and explain any new entitlement.
- Treat Docker API, registry, archive, filesystem, and network inputs as untrusted.
- Verify the signature or digest of every downloaded host or guest artifact before use. Never add an unsigned guest-image fallback.
- Keep the guest containerd socket private to the guest. Preserve users' source kubeconfig files.
- Workflows use only the automatic per-run token and SHA-pinned actions, and their checkouts do not persist credentials.

## Pull Request Evidence

Every pull request description must include an artifact that settles what the
description claims, so a reviewer can confirm the change without reproducing
it. An artifact is an inspectable result of running the change, not a summary
or an agent's report that the change works.

- Choose the artifact by what changed:
  - Behavior change or bug fix: the regression's output failing on the base revision and passing on the head.
  - CLI, headless, VM lifecycle, Docker API, or Kubernetes behavior: the commands and their actual output.
  - TUI or other visible output: before and after screenshots, terminal snapshots, or a recording.
  - Performance: the workload, the baseline, and the result measured in the same environment.
  - Release or packaging: the tested artifact's hash and the validation run that used it.
  - Documentation only: the consistency checks run and their results.
- Bind each artifact to the head commit and the environment that produced it, such as hosted CI, a physical Apple Silicon Mac, or Linux. Replace or mark it stale when the head changes.
- Put short output in the description itself. Link longer logs, CI runs, or uploaded files that reviewers can open, and remove credentials, tokens, and personal paths first.
- When an artifact cannot be produced, name the claim that stays unverified and why. Do not present it as verified.
- Also list the commands run and describe the host and guest impact.
