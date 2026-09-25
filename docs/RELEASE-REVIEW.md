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
- [ ] Isolation and cleanup: multiple profiles isolated in a disposable HOME;
  every owned profile proven stopped before its workspace is removed; source
  kubeconfig unchanged; schema 3 physical evidence bound to the exact RC.
- [ ] Compatibility: CLI/JSON break documented; external Docker connections
  preserved; only the v3 update manifest published and the reinstall path for
  v0.1.2 and earlier documented; managed-K3s profiles rejected, not migrated;
  no automatic Colima changes.
- [ ] Rollback: previous host install survives interrupted update; Docker data
  backup/recovery reviewed.
- [ ] Validation: full `make test-local-macos` passed for candidate source;
  hosted-only validation limits disclosed; any separately claimed physical
  evidence binds exact RC artifacts; no rebuilt bytes substituted after testing.
- [ ] Publication: protected promotion environment and hosted runners verified; attestations match
  repository/workflow/source/run; exact artifact hashes accepted; immutable
  release enabled; unresolved findings prevent promotion.
