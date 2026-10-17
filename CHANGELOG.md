# Changelog

## 0.3.1 (2026-10-16)

## What's Changed
* fix: stop the VM on a guest kernel panic and report it in vm status and the TUI by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/92
* fix: keep the TUI's VM detail live, quit quietly with :exit, and stop a false query cleanup error by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/94


**Full Changelog**: https://github.com/Palbahngmiyine/Hamn/compare/v0.3.0...v0.3.1

## 0.3.0 (2026-10-16)

## What's Changed
* fix: keep the TUI still during refresh and explain a stopped Hamn VM by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/60
* fix: answer a control request that arrives after its connection is accepted by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/62
* fix: keep published ports forwarded after failed requests, ended relays and a new SSH master by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/63
* fix: report a UDP port that is published on one address by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/64
* feat: apply a profile definition with vm apply and say why a profile cannot be read by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/65
* chore: reset docs and retain declarative VM regressions by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/66
* Update README.md by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/67
* ci: wait for github.com before a macOS shard checks out by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/68
* fix: bound guest readiness probes and the image check, and cancel the DHCP wait by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/70
* fix: deliver a cancellation that reaches a starting worker to its handler by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/69
* feat: report a damaged guest disk, add vm reset, and repair the root filesystem at boot by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/73
* fix: bound the guest writer, cloud-init and hook SSH clients, and cancel the vmrun wait by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/75
* feat: complete : commands in the TUI from history and known commands by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/74
* feat: describe, YAML, shell and port-forward on k9s row keys by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/72
* fix: end the vmrun wait of vm start at a monotonic deadline by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/79
* fix: end upgrade-cli timeouts from a lost signal at the updater test barrier by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/78
* fix: mount the guest root ext4 without online discard and trim it weekly by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/84
* fix: keep the vmrun identity check of vm start inside its deadline by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/81
* fix: end the vmrun status queries of vm start, retries included, at its deadline by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/85
* fix: discard unpublished guest deployment backups and retire them durably by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/80
* fix: recover a leftover transaction on cold start and reapply what a boot resets by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/86
* test: make the physical workspace-live suite pass on main again by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/83
* fix: refuse an unusable share before a stopped VM's start changes anything by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/87
* feat: complete the kinds the cluster serves after :get, custom resources included by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/77
* test: wait for the fake SSH master's accept, and give exec time to start ssh by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/88
* fix: report an empty Docker socket reply as unreadable, and deflake the relay kill wait by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/90
* fix: attach guest disks with host caching instead of the automatic mode by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/89
* chore: release 0.3.0 by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/91


**Full Changelog**: https://github.com/Palbahngmiyine/Hamn/compare/v0.2.1...v0.3.0

## 0.2.1 (2026-09-29)

## What's Changed
* fix: write bootstrap download chunks that begin with a dash by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/58


**Full Changelog**: https://github.com/Palbahngmiyine/Hamn/compare/v0.2.0...v0.2.1

## 0.2.0 (2026-09-27)

## What's Changed
* feat: improve container workflows and native upgrades by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/52
* fix: release pr-ready CRLF parsing and cancellations racing a command's start by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/53
* ci: run Release Please on the automatic token, with no stored secret by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/55
* ci: report what the release PR needs instead of dispatching CI for it by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/56


**Full Changelog**: https://github.com/Palbahngmiyine/Hamn/compare/v0.1.2...v0.2.0

## 0.1.2 (2026-09-14)

## What's Changed
* fix: improve installation UX and release manifest compatibility by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/49
* fix: install with macOS system tools and bound generation retention by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/51


**Full Changelog**: https://github.com/Palbahngmiyine/Hamn/compare/v0.1.1...v0.1.2

## 0.1.1 (2026-09-08)

## What's Changed
* docs: refine agent instructions [skip release] by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/47
* feat: add workspace-based TUI and recover interrupted Docker deployment by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/46


**Full Changelog**: https://github.com/Palbahngmiyine/Hamn/compare/v0.1.0...v0.1.1

## 0.1.0 (2026-09-06)

## What's Changed
* feat!: unify Hamn TUI and headless control in a single Rust/C binary by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/42
* fix: automate hosted releases and prepare Hamn 0.1.0 by @Palbahngmiyine in https://github.com/Palbahngmiyine/Hamn/pull/44


**Full Changelog**: https://github.com/Palbahngmiyine/Hamn/compare/v0.0.1...v0.1.0

## 0.0.1 (2026-08-18)


### Features

* automate validated release publication ([dcefde8](https://github.com/Palbahngmiyine/Hamn/commit/dcefde8db521a7b6512ba71327d81ed6f3273bef))
* gate automated release repository state ([da73ef8](https://github.com/Palbahngmiyine/Hamn/commit/da73ef86666a97e74476a0eb830e507360c592b7))
* gate releases on version manifest changes ([0612675](https://github.com/Palbahngmiyine/Hamn/commit/0612675dacae6cdcced458476d2eed963fc7169a))
* restore hosted release provenance ([14c82d7](https://github.com/Palbahngmiyine/Hamn/commit/14c82d75a561e629ff2e71b05ac88cbd906bd2e3))
* support solo local release promotion ([886f88a](https://github.com/Palbahngmiyine/Hamn/commit/886f88a8ef1b44701be3bd21283ec740884db4d9))


### Bug Fixes

* align preflight with automated tag policy ([4b47097](https://github.com/Palbahngmiyine/Hamn/commit/4b4709715ecd0819c9eb8c8f752bbc713541adf0))
* build Hamn with the system Apple SDK ([7500c50](https://github.com/Palbahngmiyine/Hamn/commit/7500c505b25e0e77b6c41a1122bf97bdcfef9a81))
* finalize Hamn 0.0.1 ([#2](https://github.com/Palbahngmiyine/Hamn/issues/2)) ([1f6264c](https://github.com/Palbahngmiyine/Hamn/commit/1f6264c3a7c5e6df0227082446897d5b26dc987e))
* ignore Nix SDK overrides on macOS ([9b4e044](https://github.com/Palbahngmiyine/Hamn/commit/9b4e044d9f01f02ffd0d3124327d719af4bd2c1a))
* keep initial release at 0.0.1 ([66e0bc2](https://github.com/Palbahngmiyine/Hamn/commit/66e0bc29f8e81b2331a34451e137955e5f66952d))
* make 0.0.1 release local-only ([a2e20b3](https://github.com/Palbahngmiyine/Hamn/commit/a2e20b35f7a3615d627df2c8a14d8bbb8f8fd754))
* pass the host SDK into Nix workflows ([e2ccfa1](https://github.com/Palbahngmiyine/Hamn/commit/e2ccfa1a2abaab3c501931eb46f589d82f7636a8))
* pass the selected SDK to clang ([6d8dae6](https://github.com/Palbahngmiyine/Hamn/commit/6d8dae6bb5718420e1dcec505ed06f5fb8ccf3e3))
* pin current Nix CI dependencies ([defd774](https://github.com/Palbahngmiyine/Hamn/commit/defd7744f16b60cd508db865624967819882a30f))
* preserve Nix validator tool paths ([10c13f4](https://github.com/Palbahngmiyine/Hamn/commit/10c13f48e87b841d372280726108e538a7746af7))
* preserve the host SDK across recursive tests ([4dfa314](https://github.com/Palbahngmiyine/Hamn/commit/4dfa31472224cc65c7ed5dad0463f870b76bca8c))
* require verified automated release commits ([f2b648a](https://github.com/Palbahngmiyine/Hamn/commit/f2b648a53b7d77b69d5ca091949eebf366ee4338))
* select the current Apple SDK in Nix shells ([ac9ffea](https://github.com/Palbahngmiyine/Hamn/commit/ac9ffea4f8d2fa91b71f5eca88f6d2e60677cdfe))
* trust sole-owner release access ([29a11c5](https://github.com/Palbahngmiyine/Hamn/commit/29a11c5180b3f8c4eaee859a127fbb81a18f0771))
* use the macOS 15 Actions image ([533fc22](https://github.com/Palbahngmiyine/Hamn/commit/533fc225c1079fc5ad4934c8c082ffdf7dfbfc02))

## Changelog

Release Please maintains this file from Conventional Commit messages.
