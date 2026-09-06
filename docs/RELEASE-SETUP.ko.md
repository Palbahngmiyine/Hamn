# 자동 릴리스와 선택적 물리 검증

영문 기준 문서는 [RELEASE-SETUP.md](RELEASE-SETUP.md)입니다.
`Palbahngmiyine/Hamn`에 필요한 설정 절차이며 특정 저장소나 runner가 검증을
통과했다는 기록은 아닙니다.

## 저장소 보호

공개 저장소에 비공개 취약점 제보·secret scanning·push protection·불변 릴리스를
활성화합니다. `main`에 PR·선형 이력·portable/macOS 검증을 요구하고 `v*` 태그의
삭제와 non-fast-forward 변경을 금지합니다. Action을 전체 commit SHA로 고정하고
기본 GITHUB_TOKEN은 읽기 전용으로 둡니다. 허용하는 Action 소유자는
`preflight-release-repository.sh` 검사와 일치해야 합니다.

`hamn-promotion` 환경을 만들고 관리자 우회를 끄며 `main`에서만 배포를
허용합니다. 환경에 secret이나 variable을 두지 않습니다. 저장소 secret은
`RELEASE_PLEASE_TOKEN`만 두며 Release Please PR에 사용합니다.

## Release Please와 0.1.0 버전

자동화 PR의 squash commit에 일회성 `Release-As: 0.1.0`을 지정합니다. 여러
커밋으로 구성된 PR #42에는 commit override를 적용하지 않습니다. 연결된 커밋마다
동일한 설명이 반복되기 때문입니다. GitHub changelog 생성 방식으로 머지된 PR을
한 번씩 요약합니다. PR #43이 manifest, version.txt, Makefile, Nix 버전을 변경합니다.
먼저 hosted 자동화 변경을 머지하고 그 다음 릴리스 PR을 머지합니다. 이후 버전은
Conventional Commit 규칙으로 증가합니다. 영구적인 `release-as` 설정은 없습니다.
직접 Cargo 빌드도 HAMN_VERSION을 지정하지 않으면 version.txt를 사용합니다.

릴리스 PR이 머지되면 GitHub-hosted Linux/macOS runner가 산출물을 빌드·검증하고
같은 바이트를 배포합니다. self-hosted runner와 `hamn-validation` 환경은 필요하지
않습니다. 불변 릴리스 게시를 확인한 뒤 해당 PR의 `autorelease: pending` 라벨을
제거하여 다음 릴리스가 막히지 않도록 합니다. PR 갱신은 Release Please workflow의
수동 실행을 사용합니다. Release workflow의 수동 실행은 아직 배포하지 않은
manifest 버전을 복구할 때만 사용합니다.

Manifest의 버전이 아직 게시되지 않았거나 draft 상태라면 Release Please는
다음 PR 생성을 보류합니다. 존재하지 않는 이전 태그로 변경 이력을 요청하지
않습니다. Release workflow가 게시와 라벨 정리를 성공하면 Release Please를
다시 실행합니다. API·인증 오류는 보류로 숨기지 않고 실패로 표시합니다.

버전 지정 규칙: [Release Please 공식 문서](https://github.com/googleapis/release-please#how-do-i-change-the-version-number).

## 선택적 수동 물리 검증

실제 VM·Docker·Kubernetes·전환 검증은 `make release-gate`로 별도 실행할 수
있으며 자동 배포의 필수 조건은 아닙니다. 격리된 Apple Silicon 장비에 macOS 개발
도구, Python 3.12 이상, Docker CLI, kubectl과 테스트용 Kubernetes context가
필요합니다. 이 선택적 로컬 검사를 위해 저장소 runner를 등록하지 않습니다.

`$HOME/.config/hamn/physical-validator.env`를 현재 사용자 소유의 일반 파일로 만들고
권한 0600, hard link 하나를 유지합니다. 수동 로컬 검증에서만 셸 코드로 읽으며 검증기
관리자만 편집할 수 있어야 합니다. 절대 경로를 지정합니다.

```sh
HAMN_E2E_CONTEXT='dedicated-test-context'
HAMN_E2E_KUBECONFIG='/absolute/path/test-kubeconfig'
HAMN_LEGACY_BINARY='/absolute/path/legacy/hamn'
HAMN_LEGACY_BINARY_SHA256='64-lowercase-hex-digest'
HAMN_LEGACY_RUNNING_FIXTURE='/absolute/path/fixtures/running'
HAMN_LEGACY_STOPPED_FIXTURE='/absolute/path/fixtures/stopped'
```

각 fixture에는 정지한 격리 구형 프로필의 `disk.img`, `config.yaml`, `id_ed25519`,
`id_ed25519.pub`, `efi-vars.bin`, `machine-id.bin`, `mac-addr`, `expected.json`이
필요합니다. 이 파일들을 커밋하지 않습니다. 설정의 `mounts`, `provision`은 빈
목록이어야 합니다. 검증기는 HOME 공유를 끄고 디스크를 복제하며 원본을 부팅하지
않습니다. `expected.json`에는 `k3sState`(`running` 또는 `stopped`)와 해당 fixture에서
실제로 수집한 `docker` 스냅샷을 기록합니다. 컨테이너·이미지·볼륨·네트워크 식별자와
검증용 볼륨 데이터 해시를 포함합니다. 정확한 스키마는 `physical_runtime.py`에
정의합니다. Docker 기본 bridge의 ID는 데몬 재시작 때 바뀌므로 기본 네트워크는
이름으로, 사용자가 만든 네트워크는 ID로 비교합니다. 실제 객체가 있는 fixture를
수집하며 빈 값이나 작성한 예상값을 증거로
쓰지 않습니다. running fixture는 K3s를 활성화한 뒤 VM을 정지하고, stopped
fixture는 K3s 데이터를 유지하되 K3s를 비활성화한 상태입니다.

검증기는 running fixture를 고정한 구형 바이너리로 시작한 뒤 후보 TUI를 실행합니다.
stopped fixture는 후보 헤드리스 인터페이스로 시작합니다. 두 경우 모두 전환 완료,
Docker 스냅샷 보존, 테스트 VM 정리를 확인해야 합니다. Kubernetes 검증기는 고유
namespace를 만들고 제거하며 원본 kubeconfig가 동일한지 확인합니다. 기본값은
클러스터 Pod 네트워크입니다. CNI가 동작하지 않는 API 검증 클러스터에서는
`HAMN_E2E_K8S_HOST_NETWORK=1`을 명시할 수 있습니다. 증거에 `podNetwork: "host"`가
기록되며 Kubernetes 조작과 로그를 검증하지만 Pod 네트워크 연결을 입증하지는
않습니다. 독립 Kubernetes 검증기에는 같은 의미의 `--host-network` 옵션이 있습니다.

## 입력과 배포 권한

`guest/image/release-inputs.json`은 Ubuntu 기반 이미지 HTTPS URL과 SHA-256을
고정합니다. 신규 이미지에는 K3s 다운로드 입력이나 호환성 서명 키가 없습니다.
GitHub artifact attestation은 산출물을 저장소·workflow·소스 commit·실행과
연결합니다. 자동 배포 증명은 hosted runner에서 생성합니다. Manifest에는
`validationMode: github-hosted-no-vm`, hosted 증거에는 `physicalE2E: false`를
기록하며 실제 VM이나 전환 E2E를 검증했다고 주장하지 않습니다.

설정 후 읽기 전용 검사를 실행합니다.

```sh
HAMN_RELEASE_REPOSITORY=Palbahngmiyine/Hamn \
  bash packaging/release/preflight-release-repository.sh
```

보호 정책, 배포 환경, 저장소 runner 부재, secret/variable 이름, Actions 권한, 불변 릴리스를
검사합니다. GitHub 상태를 바꾸거나 secret 값을 읽지 않습니다.

후보 생성 전에 [검토 체크리스트](RELEASE-REVIEW.ko.md)를 작성하고
`make test-local-macos`를 다시 실행합니다. 로컬 계약 테스트는 물리 검증이 아닙니다.
Workflow는 다음 순서로 실행합니다.

1. Hosted Linux arm64에서 게스트 이미지를 빌드하고 attest합니다.
2. Hosted macOS arm64에서 검증하고 로컬 검사를 거쳐 정확한 후보와 hosted 증거를
   만들고 attest합니다.
3. Hosted attestation과 해시를 검증한 뒤 후보를 다시 빌드하지 않고 같은 바이트를
   불변 GitHub Release에 게시합니다.

`make release-gate`에는 `RELEASE_REF`, `RELEASE_TAG`, `CANDIDATE_DIR`, 빈
`OUTPUT_DIR`와 위 검증기 입력이 필요합니다. Checkout은 깨끗해야 하며 후보 소스와
같아야 합니다. RC를 다시 빌드하지 않습니다. 검증 후 소스가 바뀌면 새 후보를 만들고
검증합니다. 물리 검사가 빠지면 수동 gate가 실패합니다. 산출물 바이트가 바뀌면
hosted 검사가 통과했더라도 자동 배포를 거부합니다.

## 설치와 호환성

공개 `install.sh`를 다운로드하고 이 저장소와 release workflow를 대상으로
`--deny-self-hosted-runners`를 지정해 GitHub attestation을 검증한 뒤 실행합니다.
설치기는 고정한 host/guest digest를 검증하고 원자적으로 게시합니다. 새 manifest를
지원하는 설치 버전은 `hamn --headless system update --yes`를 사용할 수 있습니다.
Hosted manifest는 0.0.1 릴리스에서 사용한 `github-hosted-no-vm` 검증 모드를
유지합니다.

이 릴리스는 CLI·JSON 호환성을 깨고 매니지드 K3s 클러스터 데이터·전용 로컬 볼륨을
자동 삭제합니다. Docker 객체·볼륨·사용자 마운트·원본 kubeconfig는 보존합니다.
바이너리 롤백으로 K3s 데이터를 복구할 수 없습니다. 공개 전에 이 변경을 릴리스
노트에 포함합니다.
