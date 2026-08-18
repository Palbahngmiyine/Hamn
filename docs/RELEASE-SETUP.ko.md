# Public release repository 설정

이 문서는 public `Palbahngmiyine/Hamn` repository의 영문 기준 maintainer 절차를
번역한 문서입니다. 영문 원문은 [RELEASE-SETUP.md](RELEASE-SETUP.md)입니다.

## 1. Repository 보호

Repository를 public으로 유지하고 private vulnerability reporting, secret scanning,
push protection을 활성화합니다. GitHub-owned Action, Release Please, SHA로 고정한 Nix
installer action만 허용합니다. 모든 Action은 full commit SHA로 고정하고 기본
`GITHUB_TOKEN` 권한은 read-only로 유지합니다.

`main`에는 pull request, linear history, portable/macOS status check를 요구합니다.
`v*` tag는 삭제와 non-fast-forward 변경을 금지합니다. **Settings -> General ->
Releases**에서 immutable releases를 활성화합니다.

## 2. Keyless promotion 설정

`hamn-promotion` GitHub Environment 하나만 사용합니다. Administrator bypass를 끄고
`main`만 허용하는 custom deployment branch policy를 설정합니다. 이 Environment에는
secret이나 variable을 추가하지 않습니다.

Release 자동화를 위한 self-hosted runner를 등록하지 않습니다. Release는 GitHub-hosted
Ubuntu arm64 및 macOS arm64 runner만 사용합니다. Guest-image URL, release public key,
validator identity, validator key, release signing key는 필요하지 않습니다.

Repository secret은 `RELEASE_PLEASE_TOKEN` 하나만 유지합니다. 이 token은 Release Please가
일반 repository check를 거쳐 release pull request를 갱신하는 데만 사용합니다. Release
artifact의 build, signing, attestation, publish에는 사용하지 않습니다.

## 3. Build input을 source에 고정

`guest/image/release-inputs.json`은 Ubuntu base image와 K3s input의 HTTPS URL 및 SHA-256
digest를 기록합니다. 이 파일의 변경을 source 변경처럼 검토합니다. Hosted Linux job은
다운로드한 파일의 digest가 다르면 중단합니다.

Linux job은 해당 guest image에 포함되는 K3s compatibility manifest를 결합하기 위해
일회성 Ed25519 key를 생성합니다. Job이 끝나기 전에 private/public key 파일을 모두
삭제합니다. 이 key는 stable release identity가 아니며 GitHub에 저장되지 않습니다.

Release provenance는 GitHub artifact attestation으로 제공합니다. 각 attestation은 job의
단기 OIDC identity를 사용합니다. Promotion 전 repository, `.github/workflows/release.yml`,
release commit, GitHub-hosted runner를 모두 확인합니다.

## 4. Repository 준비 상태 검증

GitHub 설정을 마친 뒤 read-only preflight를 실행합니다.

```sh
HAMN_RELEASE_REPOSITORY=Palbahngmiyine/Hamn \
  bash packaging/release/preflight-release-repository.sh
```

Repository 소유권과 공개 상태, 활성 workflow, Actions 권한, self-hosted runner와 release
variable의 부재, secret 이름 경계, `main` 전용 promotion Environment, ruleset, immutable
releases, private vulnerability reporting을 검증합니다. Secret 값은 읽지 않고 GitHub
상태도 변경하지 않습니다.

## 5. `v0.0.1` 공개

Release Please pull request를 merge하면 release manifest가 갱신되고 release workflow가
시작됩니다. 자동 실행을 복구해야 할 때에는 정확한 현재 `main` commit에서 workflow를
수동 실행할 수 있습니다. 임의의 tag, commit, 이전 workflow run은 허용하지 않습니다.

Workflow는 promotion할 byte를 다시 build하지 않고 다음 단계를 수행합니다.

1. GitHub-hosted Ubuntu arm64 job이 고정된 input을 다운로드하고 completed guest image를
   build 및 attest한 뒤 workflow artifact로 전달합니다.
2. GitHub-hosted macOS arm64 job이 attestation을 검증하고 local source/package gate를
   실행한 뒤 exact candidate와 hosted validation evidence를 만들고 모든 candidate
   artifact를 attest합니다.
3. `hamn-promotion` job이 candidate/evidence attestation을 검증하고 stable manifest를
   생성합니다. Exact candidate byte를 draft GitHub Release에 upload한 뒤 공개합니다.
4. Release가 `isImmutable: true`인지 확인하고, 다시 다운로드한 모든 asset을 GitHub에
   기록된 digest 및 release attestation과 대조합니다.

Evidence에는 `physicalE2E`, `vmLifecycle`, `dockerE2E`, `k3sE2E`가 false로 명시됩니다.
Release는 실제 Hamn VM test를 수행했다고 주장하지 않습니다. 남아 있는 physical E2E
harness는 maintainer가 선택적으로 쓰는 도구이며 자동 release authority에 포함되지 않습니다.

## 6. 설치

가장 짧은 지원 설치 명령은 다음과 같습니다.

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

이 편의 경로는 최초 installer byte에 대해 GitHub HTTPS와 repository control을 신뢰합니다.
Installer가 시작된 뒤에는 version에 고정된 host/guest artifact의 embedded SHA-256 digest를
검증하고 원자적으로 설치합니다.

Bootstrap 자체도 독립 검증하려면 먼저 다운로드하고 GitHub attestation을 검증한 뒤
실행합니다.

```sh
curl -fsSLO --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh
gh attestation verify install.sh \
  --repo Palbahngmiyine/Hamn \
  --signer-workflow Palbahngmiyine/Hamn/.github/workflows/release.yml \
  --deny-self-hosted-runners
/bin/bash install.sh
```
