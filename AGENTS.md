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
