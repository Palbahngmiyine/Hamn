# Automated releases and optional physical validation

See [RELEASE-SETUP.ko.md](RELEASE-SETUP.ko.md) for Korean. This procedure
applies to `Palbahngmiyine/Hamn`; it describes required configuration, not a
claim that a particular repository or runner has passed validation.

## Repository protection

Keep the repository public with private vulnerability reporting, secret
scanning, push protection, and immutable releases enabled. Require pull
requests, linear history, and portable/macOS checks on `main`; protect `v*`
tags against deletion and non-fast-forward changes. Pin Actions to full commit
SHAs and keep the default GITHUB_TOKEN read-only. Allow only the Actions
owners checked by `preflight-release-repository.sh`.

Create the `hamn-promotion` environment. Disable admin bypass and allow
only `main`, without environment secrets or variables. `RELEASE_PLEASE_TOKEN`
is the only repository secret and is used for Release Please PRs.

## Release Please and version 0.1.0

The automation PR squash commit uses the one-time `Release-As: 0.1.0` footer.
Do not put a commit override on the multi-commit PR #42: it repeats the same
message for each associated commit. GitHub-generated changelogs summarize
merged PRs once. Release Please PR #43 updates the manifest, version.txt, Makefile,
and Nix version to 0.1.0. Merge the hosted automation changes before merging
that release PR. Later releases use normal Conventional Commit increments;
there is no permanent `release-as` configuration to pin future versions.
Direct Cargo builds also read version.txt unless HAMN_VERSION is supplied.

After the release PR merges, GitHub-hosted Linux/macOS runners build and
validate the artifacts, then publish those same bytes. No self-hosted runner
or `hamn-validation` environment is required. Publication verifies the immutable
release before clearing its release PR's `autorelease: pending` label, so the
next release is not blocked. Use the Release Please workflow's manual dispatch
to refresh a PR; use Release's manual dispatch only to recover an unpublished
manifest version.

While that manifest version is unpublished or still a draft, Release Please
defers the next PR instead of requesting notes against a missing previous tag.
A successful Release workflow runs Release Please again after publication and
label completion. API/authentication failures still fail the check visibly.

Version override semantics: [Release Please documentation](https://github.com/googleapis/release-please#how-do-i-change-the-version-number).

## Optional manual physical validation

Physical VM, Docker, Kubernetes and migration E2E checks remain available through
`make release-gate`. They are not an automatic publication prerequisite. To run
them manually, use an isolated Apple Silicon machine with macOS development
tools, Python 3.12+, Docker CLI, kubectl and a disposable Kubernetes context.
Do not register a repository runner for this optional local check.

Provide `$HOME/.config/hamn/physical-validator.env` as an owned regular file
with mode 0600 and one hard link. Source this file only for the optional local gate;
only the validator administrator may edit it. Define absolute paths:

```sh
HAMN_E2E_CONTEXT='dedicated-test-context'
HAMN_E2E_KUBECONFIG='/absolute/path/test-kubeconfig'
HAMN_LEGACY_BINARY='/absolute/path/legacy/hamn'
HAMN_LEGACY_BINARY_SHA256='64-lowercase-hex-digest'
HAMN_LEGACY_RUNNING_FIXTURE='/absolute/path/fixtures/running'
HAMN_LEGACY_STOPPED_FIXTURE='/absolute/path/fixtures/stopped'
```

Each fixture directory contains a stopped, isolated legacy profile's
`disk.img`, `config.yaml`, `id_ed25519`, `id_ed25519.pub`, `efi-vars.bin`,
`machine-id.bin`, `mac-addr`, and `expected.json`. Never commit these files.
The config must have empty `mounts` and `provision` lists. The harness clones
fixture disks and never boots the originals. Only the running clone enables
`mountHome`, sharing the harness's newly created private temporary HOME;
the real user HOME is never shared. This preserves the old default input:
v0.0.1 emits invalid null cloud-init mounts when all sharing is disabled.
The stopped clone keeps HOME sharing disabled. No seed, deployment marker,
guest source, or original fixture is changed to bypass legacy startup checks.
`expected.json` records `k3sState` (`running` or `stopped`) and a `docker`
snapshot captured from that fixture, including container/image/volume/network
identities and sentinel volume content hashes. See `physical_runtime.py` for
the snapshot schema. Docker recreates its built-in bridge identity on daemon
restart: check built-in networks by name and user-created networks by ID.
Capture real populated fixtures; empty or invented
snapshots are not evidence. The running fixture enables K3s before shutdown;
the stopped fixture contains K3s data but has K3s disabled.

The harness starts the running fixture with the pinned legacy binary before
launching and quitting the candidate TUI. It verifies that TUI entry leaves
the VM state, pending migration and last operation unchanged. It independently
waits for the guest K3s service to be active/enabled and its API ready, then explicitly
confirms `hamn --headless vm migrate --profile <clone> --yes`.
It starts the stopped fixture with the candidate
headless interface. Both must finish retirement, preserve Docker snapshots,
and leave no live test VM. The Kubernetes harness creates and removes a
unique test namespace and verifies the source kubeconfig is unchanged.
The release gate defaults to the cluster Pod network. For an API validation
cluster without working CNI, explicitly set `HAMN_E2E_K8S_HOST_NETWORK=1`.
The evidence records `podNetwork: "host"`; this validates Kubernetes operations
and logs but does not establish Pod network connectivity. The standalone
Kubernetes harness accepts the equivalent `--host-network` option.

## Inputs and release authority

`guest/image/release-inputs.json` pins the Ubuntu base HTTPS URL and SHA-256.
There are no K3s download inputs or compatibility signing keys for new images.
GitHub artifact attestations bind built artifacts to repository, workflow,
source commit, and run. Automated release provenance comes from hosted runners.
The manifest records `validationMode: github-hosted-no-vm`; hosted evidence
records `physicalE2E: false` and does not claim real VM or migration E2E.
Publication validates the v2 and v3 manifests with the executable from the
digest-verified candidate archive, the same parser installed clients use, and
requires both manifests to name identical artifacts.

Run the read-only settings check after configuration:

```sh
HAMN_RELEASE_REPOSITORY=Palbahngmiyine/Hamn \
  bash packaging/release/preflight-release-repository.sh
```

It checks protections, the promotion environment, absence of repository runners, secret/variable names,
Actions permissions, and immutable releases. It does not modify GitHub state
or read secret values.

Before assembling a candidate, complete [the review checklist](RELEASE-REVIEW.md)
and rerun `make test-local-macos`. A local contract test is not physical proof.
The release workflow then:

1. Builds and attests the guest image on hosted Linux arm64.
2. Verifies it on hosted macOS arm64, runs local gates, assembles the exact
   candidate, and attests candidate bytes and hosted evidence.
3. Verifies hosted attestations and hashes, then uploads the same candidate
   bytes to an immutable GitHub Release without rebuilding.

The ephemeral Linux builder removes optional `passt` so libguestfs consistently
uses QEMU SLIRP networking. Image assembly checks DNS with a 30-second deadline
before package installation; runner-image networking changes must fail visibly.

### Guest image size evidence

The trusted Linux arm64 builder also needs `build-essential` and `zlib1g-dev`:
it compiles the same qcow2 decoder shipped in the host and compares extracted
raw SHA-256 with `qemu-img`. `guest/image/build-ubuntu-24.04-arm64.sh` captures
a compressed baseline from the provisioned filesystem before removing build
packages and regenerable content. Both measurements therefore start from one
provisioned package set; this does not pin repository packages across separate
builds. Cleanup protects runtime packages, purges gcc/make and unused build
dependencies, resets first-boot state, and discards free filesystem blocks.
The compact image uses zlib and retains an 8 GiB virtual disk. Byte equivalence
applies between the cleaned stage and its compact representation; deleting
packages intentionally changes bytes relative to the pre-cleanup baseline.

The builder requires savings of at least `max(64 MiB, 5%)`, a compressed size
below 2 GiB, decoder/reference equality, protective MBR and GPT CRC checks.
It writes `<image>.size-report.json` and package inventories at
`<image>.packages-before.tsv` and `<image>.packages-after.tsv`. The report binds
actual byte counts and hashes to the base digest and source revision. These
artifacts accompany the image in release evidence and attestation.

A measurement-only source snapshot establishes evidence for that snapshot, not
for a later PR revision or release candidate. Publication requires the report's
`sourceRevision` to equal the candidate commit as well as matching the exact image
size and digest. After the release source or budget changes, build and validate
the release artifacts from that final commit. Do not relabel an earlier report
or reuse runtime evidence for different image or host bytes.

The first measured result needs human review before creating
`guest/image/release-size-budget.json`. On the isolated builder, set
`HAMN_GUEST_SIZE_REVIEW_ONLY=1` alongside the normal base image/hash/output
inputs to produce a review candidate and
`<image>.size-report.budget-proposal.json`. This mode still enforces minimum
savings and all structural checks, but its `reviewOnly: true` report cannot be
published. After reviewing the actual footprint and runtime evidence, commit
the approved proposal as the budget and build a normal candidate. Budget
increases require a new footprint review; missing budgets fail closed.
Publication independently verifies the exact image/report and the report's
source revision with
`guest/build/hamn-image-tool verify-release-size IMAGE SIZE_REPORT REVIEWED_BUDGET COMMIT`,
a C tool built by `make -C guest image-tool`.

For actual before/after runtime validation, also set
`HAMN_GUEST_BASELINE_OUTPUT=/owned/output/hamn-baseline.img` during that build.
The optional export preserves the provisioned image before cleanup and its
`.sha256` sidecar with mode 0600. It verifies the baseline against the size
report and publishes complete files only after the existing image gates pass.
Existing outputs, symlinks, directories writable by other users, and overlaps with candidate
artifacts are rejected. The baseline is review evidence, not a release asset.

To generate two reproducible cleanup variations on the Linux arm64 builder:

```sh
make -C guest image-tool
guest/build/hamn-image-tool variations --source-root . \
  --baseline /owned/output/hamn-baseline.img \
  --size-report /owned/output/hamn-guest.img.size-report.json \
  --output-directory /owned/output/variations --seed 20260921
```

The output directory must not exist. The fixture varies disposable logs, apt
cache files, temporary files, and first-boot identities; it reruns cleanup,
package-inventory comparison, compaction, decoder/reference, GPT and size gates.
`variations.json` binds each image digest to its generated inputs and marks
physical runtime validation pending. Boot the baseline, normal candidate and
each generated image in separately owned disposable profiles before claiming
functional equivalence; structural success alone does not satisfy that check.

Hosted structural checks do not establish boot or functional equivalence.
Physical validation remains necessary for this image-optimization acceptance,
even though the automated publication workflow does not run that optional manual gate.
Acceptance evidence for the optimized image must separately cover Docker API,
CLI, Compose, Buildx, containerd/runc/CNI, amd64 binfmt, opt-in Rosetta, external
Kubernetes connectivity, legacy K3s retirement and reboot data preservation on
the exact artifact. Local tests with synthetic
size fixtures or sparse disks do not establish image-size savings or VM
behavior. Do not substitute a fabricated baseline or budget when that Linux
build or physical validation has not run.

`make release-gate` takes `RELEASE_REF`, `RELEASE_TAG`, `CANDIDATE_DIR`, and an
empty `OUTPUT_DIR`, plus the validator inputs above. Checkout must be clean
and match the candidate source. It never rebuilds the RC. If source changes
after validation, assemble and validate a new candidate; do not reuse evidence.
Missing physical checks prevent a successful manual gate. Changed artifact bytes
prevent automatic publication, even when hosted tests passed.

## Installation and compatibility

Download the published `install.sh`, verify its GitHub attestation against
this repository and release workflow with `--deny-self-hosted-runners`, then
execute it. The installer validates pinned host/guest digests and publishes
atomically. Installed versions supporting the new manifest can run
`hamn --headless system update --yes`. The hosted manifest retains the `github-hosted-no-vm` validation mode used by
the 0.0.1 release.

This release breaks CLI/JSON compatibility and automatically removes managed
K3s cluster data and dedicated local volumes. Docker objects and volumes,
user mounts, and original kubeconfig are preserved. Binary rollback cannot
recover K3s data. Include this warning in release notes before publication.
