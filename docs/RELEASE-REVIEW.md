# Release candidate human review

Complete this checklist for each candidate before promotion. Record reviewer,
source commit/tree, candidate manifest SHA-256, exact binary SHA-256, date,
commands, evidence paths, and unresolved findings. An unchecked item is not a
pass. See [Korean](RELEASE-REVIEW.ko.md).

- [ ] User behavior: no-argument TUI and non-TTY error; matching headless and
  TUI operations; visible profile/context/namespace; complete mutation impact
  confirmation; Korean text, resize, help, navigation and logs.
- [ ] Terminal/process lifecycle: Ctrl-C, SIGTERM, panic, suspend/resume and
  terminal restoration; TUI exit leaves the owned VM running; stale responses
  cannot change the current target; interrupted mutations report uncertainty.
- [ ] Security: one Mach-O, expected system-library dependencies, valid
  signature and only expected entitlements; image/provenance checks reject
  altered inputs; profile paths/locks/worker protocol fail closed.
- [ ] API behavior: Docker without Docker CLI; external socket client access;
  external Kubernetes auth, permission errors and disconnected watches;
  source kubeconfig unchanged; stale object identities reject mutations.
- [ ] Performance: record startup, CPU/memory during refresh and logs, terminal
  input responsiveness during network delays, and bounded stream behavior.
- [ ] Migration: real populated running/stopped legacy fixtures; interrupted
  steps resume; K3s service/data removed; Docker object identities and volume
  hashes unchanged; multiple profiles isolated; cleanup complete.
- [ ] Compatibility: CLI/JSON break documented; external Docker connections
  preserved; old updater bootstrap path documented; no automatic Colima changes.
- [ ] Rollback: previous host install survives interrupted update; Docker data
  backup/recovery reviewed; release notes explicitly say K3s deletion cannot
  be reversed by binary rollback.
- [ ] Validation: full `make test-local-macos` passed for candidate source;
  hosted-only validation limits disclosed; any separately claimed physical
  evidence binds exact RC artifacts; no rebuilt bytes substituted after testing.
- [ ] Publication: protected promotion environment and hosted runners verified; attestations match
  repository/workflow/source/run; exact artifact hashes accepted; immutable
  release enabled; unresolved findings prevent promotion.
