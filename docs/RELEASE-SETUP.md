# Public release repository setup

This is the canonical English maintainer procedure for creating the public
`Palbahngmiyine/hamn` repository before the first RC. See
[RELEASE-SETUP.ko.md](RELEASE-SETUP.ko.md) for Korean.

## 1. Create the public source repository

Create an empty public repository named `Palbahngmiyine/hamn`. Preserve the
existing private history separately as `Palbahngmiyine/hamn-matrix`; do not
push that history to the public repository.

Export the reviewed source tree with:

```sh
packaging/release/export-public-source.sh /absolute/path/to/public-hamn
cd /absolute/path/to/public-hamn
git remote add origin https://github.com/Palbahngmiyine/hamn.git
git log --all --oneline
git push -u origin main
```

The log must show one root commit only. Before pushing, run:

```sh
make -j1 test-public-export
make -j1 test-core-quality
make -j1 test-local-macos
```

Enable private vulnerability reporting in the repository Security settings so
that [the security policy](../SECURITY.md) has a private reporting route.

## 2. Configure release protection

Create these GitHub Environments:

- `hamn-validation` for the physical Apple Silicon validation job. Require
  manual reviewer approval before the validator private key is supplied.
- `hamn-promotion` for stable promotion. Require manual reviewer approval
  before the release private key is supplied.

Register one dedicated, resettable physical Mac runner for validation. It must
be online and have the labels `self-hosted`, `macOS`, `ARM64`, and
`hamn-validator`. It must use a clean workspace and must not contain a user
Hamn or Colima installation that the release job could modify.

## 3. Supply release inputs without committing them

Create the immutable Ubuntu 24.04 arm64 guest artifact on a trusted Linux arm64
image builder. Publish it at an HTTPS URL reachable by the hosted candidate
runner, then configure these repository variables:

| Variable | Value |
| --- | --- |
| `HAMN_GUEST_IMAGE_URL` | HTTPS URL of the immutable Hamn guest artifact. |
| `HAMN_GUEST_IMAGE_SHA256` | Lowercase SHA-256 of that exact artifact. |
| `HAMN_VALIDATOR_IDENTITY` | Stable non-secret name of the physical validator. |
| `HAMN_RELEASE_PUBLIC_KEY` | Release signing public key. |
| `HAMN_VALIDATOR_PUBLIC_KEY` | Validator signing public key. |

Create an Ed25519 release signing key and a distinct Ed25519 validator signing
key. Store the private keys only as the named secrets of their respective
protected GitHub Environments:

| Environment | Secret | Content |
| --- | --- |
| `hamn-validation` | `HAMN_VALIDATOR_SIGNING_KEY` | Validator signing private key. |
| `hamn-promotion` | `HAMN_RELEASE_SIGNING_KEY` | Release signing private key. |

Never commit key files, paste a private key into an issue, or reuse the release
and validator key pairs. Back up each private key in an owner-controlled
encrypted vault before upload. The hosted candidate receives only the public
release key; the physical validator and promotion steps create private
temporary files with restricted permissions and remove them after use.

The hosted candidate also creates a keyless GitHub artifact attestation for
every file named by its `SHA256SUMS`. It uses a short-lived OIDC identity, not
either signing key. Maintainers can independently inspect a released artifact
with `gh attestation verify <artifact> --repo Palbahngmiyine/hamn`.

In **Settings → Actions → General**, restrict the repository to GitHub-owned
actions, require full-length commit-SHA pins, and keep the default
`GITHUB_TOKEN` permission read-only. The workflow already requests write
permissions only for the approved promotion job.

## 4. Verify repository readiness

Run the read-only preflight after GitHub configuration is complete:

```sh
HAMN_RELEASE_REPOSITORY=Palbahngmiyine/hamn \
  bash packaging/release/preflight-release-repository.sh
```

It verifies public visibility, the sole release workflow, both Environments,
manual promotion approval, the online validator runner, the GitHub-owned
SHA-pinned Actions policy, and all required variable and secret names. It
cannot read secret values and does not change GitHub state.

## 5. Publish without rebuilding stable bytes

Push `v0.0.1-rc.N` to create the arm64 candidate. The physical validator must
run the exact downloaded candidate and upload signed E2E evidence. After the
evidence passes, manually dispatch the promotion workflow with that RC tag and
the stable tag `v0.0.1`.

Promotion verifies the evidence and hashes, creates the signed canonical
manifest, and uploads the exact validated host archive, guest image, installer,
SBOM, manifest, manifest signature, and validation evidence to the stable
GitHub Release. It does not rebuild the host binary or guest image.
