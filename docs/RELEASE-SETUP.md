# Release repository and physical validator

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

Create both `hamn-promotion` and `hamn-validation` environments. Disable admin
bypass and allow deployments only from `main`. Neither environment has secrets
or variables. `RELEASE_PLEASE_TOKEN` is the only repository secret; it is used
for Release Please PRs, not release signing or validation.

## Physical runner

Register one online macOS runner with labels `self-hosted`, `macOS`, `ARM64`,
and `hamn-validator`. It must be a physical Apple Silicon machine with
Virtualization.framework available, macOS development tools, Python 3.12 or
later, Docker CLI, kubectl, and access to an explicitly designated disposable
Kubernetes test environment. Do not assign public pull-request jobs to it.
The release validation job has no release write permission.

Provide `$HOME/.config/hamn/physical-validator.env` as an owned regular file
with mode 0600 and one hard link. The workflow sources this file as shell code;
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
The config must have empty `mounts` and `provision` lists. The harness disables
HOME sharing and clones fixture disks; it never boots the originals.
`expected.json` records `k3sState` (`running` or `stopped`) and a `docker`
snapshot captured from that fixture, including container/image/volume/network
identities and sentinel volume content hashes. See `physical_runtime.py` for
the snapshot schema. Docker recreates its built-in bridge identity on daemon
restart: check built-in networks by name and user-created networks by ID.
Capture real populated fixtures; empty or invented
snapshots are not evidence. The running fixture enables K3s before shutdown;
the stopped fixture contains K3s data but has K3s disabled.

The harness starts the running fixture with the pinned legacy binary before
launching the candidate TUI. It starts the stopped fixture with the candidate
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
source commit, and run. Build provenance must come from hosted runners;
physical proof comes from the designated self-hosted validator.

Run the read-only settings check after configuration:

```sh
HAMN_RELEASE_REPOSITORY=Palbahngmiyine/Hamn \
  bash packaging/release/preflight-release-repository.sh
```

It checks protections, both environments, runner labels, secret/variable names,
Actions permissions, and immutable releases. It does not modify GitHub state
or read secret values.

Before assembling a candidate, complete [the review checklist](RELEASE-REVIEW.md)
and rerun `make test-local-macos`. A local contract test is not physical proof.
The release workflow then:

1. Builds and attests the guest image on hosted Linux arm64.
2. Verifies it on hosted macOS arm64, runs local gates, assembles the exact
   candidate, and attests candidate bytes and hosted evidence.
3. Verifies those attestations on the physical runner and executes the harness
   extracted from that candidate. It attests `physical-validation-evidence.json`.
4. Requires both hosted and physical evidence in promotion, verifies provenance
   and hashes, and uploads the same bytes to an immutable GitHub Release.

`make release-gate` takes `RELEASE_REF`, `RELEASE_TAG`, `CANDIDATE_DIR`, and an
empty `OUTPUT_DIR`, plus the validator inputs above. Checkout must be clean
and match the candidate source. It never rebuilds the RC. If source changes
after validation, assemble and validate a new candidate; do not reuse evidence.
Missing physical checks or changed artifact bytes prevent publication.

## Installation and compatibility

Download the published `install.sh`, verify its GitHub attestation against
this repository and release workflow with `--deny-self-hosted-runners`, then
execute it. The installer validates pinned host/guest digests and publishes
atomically. Installed versions supporting the new manifest can run
`hamn --headless system update --yes`. Older updater versions that reject
`physical-apple-silicon` manifests must use the new verified installer.

This release breaks CLI/JSON compatibility and automatically removes managed
K3s cluster data and dedicated local volumes. Docker objects and volumes,
user mounts, and original kubeconfig are preserved. Binary rollback cannot
recover K3s data. Include this warning in release notes before publication.
