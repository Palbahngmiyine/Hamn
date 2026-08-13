# Public release repository 설정

이 문서는 첫 RC 전에 public `Palbahngmiyine/hamn` repository를 만드는 영문 기준
maintainer 절차의 한국어 번역입니다. 영문 원문은
[RELEASE-SETUP.md](RELEASE-SETUP.md)입니다.

## 1. Public source repository 생성

비어 있는 public repository `Palbahngmiyine/hamn`을 만듭니다. 기존 private history는
`Palbahngmiyine/hamn-matrix`로 별도 보존하고 public repository로 push하지 마세요.

검토한 source tree는 다음으로 export합니다.

```sh
packaging/release/export-public-source.sh /absolute/path/to/public-hamn
cd /absolute/path/to/public-hamn
git remote add origin https://github.com/Palbahngmiyine/hamn.git
git log --all --oneline
git push -u origin main
```

Log에는 root commit 하나만 있어야 합니다. Push 전에 다음을 실행하세요.

```sh
make -j1 test-public-export
make -j1 test-core-quality
make -j1 test-local-macos
```

Repository Security setting에서 private vulnerability reporting을 활성화하여
[보안 정책](../SECURITY.md)에 private reporting 경로가 있도록 합니다.

## 2. Release 보호 설정

다음 GitHub Environment를 만듭니다.

- 물리 Apple Silicon validation job용 `hamn-validation`. Validator private key가
  제공되기 전에 manual reviewer approval을 요구합니다.
- Stable promotion용 `hamn-promotion`. Release private key가 제공되기 전에 manual
  reviewer approval을 요구합니다.

Validation에는 dedicated, resettable physical Mac runner 하나를 등록합니다. Runner는
online 상태이고 `self-hosted`, `macOS`, `ARM64`, `hamn-validator` label을 가져야 합니다.
Clean workspace를 사용해야 하며 release job이 바꿀 수 있는 사용자 Hamn 또는 Colima
installation이 있어서는 안 됩니다.

## 3. Release input을 commit하지 않고 제공

Trusted Linux arm64 image builder에서 immutable Ubuntu 24.04 arm64 guest artifact를
만듭니다. Hosted candidate runner가 접근할 수 있는 HTTPS URL에 올리고, 다음 repository
variable을 설정합니다.

| Variable | 값 |
| --- | --- |
| `HAMN_GUEST_IMAGE_URL` | Immutable Hamn guest artifact의 HTTPS URL |
| `HAMN_GUEST_IMAGE_SHA256` | 정확히 그 artifact의 lowercase SHA-256 |
| `HAMN_VALIDATOR_IDENTITY` | 물리 validator의 stable non-secret 이름 |
| `HAMN_RELEASE_PUBLIC_KEY` | Release signing public key |
| `HAMN_VALIDATOR_PUBLIC_KEY` | Validator signing public key |

Ed25519 release signing key와 별개의 Ed25519 validator signing key를 만듭니다. Private
key는 각각의 protected GitHub Environment에만 다음 이름으로 저장합니다.

| Environment | Secret | 내용 |
| --- | --- |
| `hamn-validation` | `HAMN_VALIDATOR_SIGNING_KEY` | Validator signing private key |
| `hamn-promotion` | `HAMN_RELEASE_SIGNING_KEY` | Release signing private key |

Key file을 commit하거나 private key를 issue에 붙이지 말고, release와 validator key pair를
재사용하지 마세요. Upload 전 private key는 owner가 제어하는 암호화된 vault에 백업합니다.
Hosted candidate에는 public release key만 전달합니다. Physical validator와 promotion step은
권한을 제한한 temporary file을 만들고 사용 뒤 제거합니다.

Hosted candidate는 `SHA256SUMS`에 이름이 있는 모든 파일에 대해 keyless GitHub artifact
attestation도 만듭니다. 여기에는 두 signing key 대신 짧은 수명의 OIDC identity를 사용합니다.
Maintainer는 `gh attestation verify <artifact> --repo Palbahngmiyine/hamn`으로 배포된
artifact를 독립적으로 확인할 수 있습니다.

**Settings → Actions → General**에서 repository가 GitHub-owned action만 사용하도록 제한하고,
full-length commit SHA pin을 요구하며, 기본 `GITHUB_TOKEN` 권한은 read-only로 유지하세요.
Workflow는 승인된 promotion job에서만 write 권한을 요청합니다.

## 4. Repository 준비 상태 검증

GitHub 설정을 마친 뒤 read-only preflight를 실행합니다.

```sh
HAMN_RELEASE_REPOSITORY=Palbahngmiyine/hamn \
  bash packaging/release/preflight-release-repository.sh
```

이 명령은 public visibility, 하나뿐인 release workflow, 두 Environment, manual promotion
approval, online validator runner, GitHub-owned SHA-pinned Actions policy, 필요한
variable/secret 이름을 검증합니다. Secret 값은 읽지 않으며 GitHub 상태를 바꾸지 않습니다.

## 5. Stable bytes를 rebuild하지 않고 공개

`v0.0.1-rc.N`을 push해 arm64 candidate를 만듭니다. Physical validator는 다운로드한 정확한
candidate를 실행하고 signed E2E evidence를 upload해야 합니다. Evidence가 통과하면 그 RC
tag와 stable tag `v0.0.1`로 promotion workflow를 수동 실행합니다.

Promotion은 evidence와 hash를 검증하고 signed canonical manifest를 만든 뒤, 검증한 정확한
host archive, guest image, installer, SBOM, manifest, manifest signature, validation evidence를
stable GitHub Release에 upload합니다. Host binary나 guest image를 다시 build하지 않습니다.
