# Public release repository setup

This is the canonical maintainer procedure for the public
`Palbahngmiyine/Hamn` repository. See
[RELEASE-SETUP.ko.md](RELEASE-SETUP.ko.md) for Korean.

## 1. Protect the repository

Keep the repository public and enable private vulnerability reporting, secret
scanning, and push protection. Permit only GitHub-owned Actions, Release
Please, and the pinned Nix installer action. Require full commit-SHA pins and
keep the default `GITHUB_TOKEN` permission read-only.

Protect `main` with pull requests, linear history, and the portable and macOS
status checks. Protect `v*` tags from deletion and non-fast-forward updates.
Enable immutable releases in **Settings -> General -> Releases**.

## 2. Configure keyless promotion

Use one GitHub Environment named `hamn-promotion`. Disable administrator
bypass and configure a custom deployment branch policy that permits only
`main`. Do not add secrets or variables to this environment.

Do not register a self-hosted runner for release automation. The release uses
GitHub-hosted Ubuntu arm64 and macOS arm64 runners exclusively. The repository
needs no guest-image URL, release public key, validator identity, validator
key, or release signing key.

`RELEASE_PLEASE_TOKEN` remains the only repository secret. It is used only to
let Release Please update its release pull request through the normal
repository checks. It is not used to build, sign, attest, or publish release
artifacts.

## 3. Pin build inputs in source

`guest/image/release-inputs.json` records the HTTPS URLs and SHA-256 digests of
the Ubuntu base image and K3s inputs. Review changes to that file like source
changes. The hosted Linux job rejects a download whose digest differs.

The Linux job creates an ephemeral Ed25519 key only to bind the K3s
compatibility manifest embedded in that one guest image. It deletes the
private and public key files before the job ends. This key is not a stable
release identity and is not stored in GitHub.

GitHub artifact attestations provide release provenance. Each attestation uses
the job's short-lived OIDC identity and is verified against this repository,
`.github/workflows/release.yml`, the release commit, and a GitHub-hosted runner
before promotion.

## 4. Verify repository readiness

Run the read-only preflight after the GitHub settings are complete:

```sh
HAMN_RELEASE_REPOSITORY=Palbahngmiyine/Hamn \
  bash packaging/release/preflight-release-repository.sh
```

It verifies repository ownership and visibility, active workflows, Actions
permissions, the absence of self-hosted runners and release variables, the
secret-name boundary, the `main`-only promotion environment, rulesets,
immutable releases, and private vulnerability reporting. It never reads a
secret value or changes GitHub state.

## 5. Publish `v0.0.1`

Merging the Release Please pull request updates the release manifest. That
change starts the release workflow. If the automatic run needs recovery, a
maintainer may manually dispatch the workflow on the exact current `main`
commit; arbitrary tags, commits, and earlier workflow runs are not accepted.

The workflow performs these steps without rebuilding promoted bytes:

1. A GitHub-hosted Ubuntu arm64 job downloads pinned inputs, builds the
   completed guest image, attests it, and transfers it as a workflow artifact.
2. A GitHub-hosted macOS arm64 job verifies that attestation, runs the local
   source and packaging gates, builds the exact candidate, records hosted
   validation evidence, and attests every candidate artifact.
3. The `hamn-promotion` job verifies the candidate and evidence attestations,
   creates the stable manifest, uploads the exact candidate bytes to a draft
   GitHub Release, and then publishes it.
4. The job requires the release to report `isImmutable: true`, verifies every
   downloaded asset against GitHub's recorded digest, and verifies the release
   attestation.

The evidence explicitly records `physicalE2E`, `vmLifecycle`, `dockerE2E`, and
`k3sE2E` as false. The release does not claim a live Hamn VM test. The retained
physical E2E harness is optional maintainer tooling and is not part of the
automated release authority.

## 6. Install

The shortest supported installation is:

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

This convenience path initially trusts GitHub HTTPS and repository control for
the installer bytes. After it starts, the version-pinned installer verifies the
embedded SHA-256 digests before atomically installing the host and guest
artifacts.

For an independently verified bootstrap, download first and verify its GitHub
attestation before execution:

```sh
curl -fsSLO --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh
gh attestation verify install.sh \
  --repo Palbahngmiyine/Hamn \
  --signer-workflow Palbahngmiyine/Hamn/.github/workflows/release.yml \
  --deny-self-hosted-runners
/bin/bash install.sh
```
