# 릴리스 저장소와 물리 검증기

영문 기준 문서는 [RELEASE-SETUP.md](RELEASE-SETUP.md)입니다.
`Palbahngmiyine/Hamn`에 필요한 설정 절차이며 특정 저장소나 runner가 검증을
통과했다는 기록은 아닙니다.

## 저장소 보호

공개 저장소에 비공개 취약점 제보·secret scanning·push protection·불변 릴리스를
활성화합니다. `main`에 PR·선형 이력·portable/macOS 검증을 요구하고 `v*` 태그의
삭제와 non-fast-forward 변경을 금지합니다. Action을 전체 commit SHA로 고정하고
기본 GITHUB_TOKEN은 읽기 전용으로 둡니다. 허용하는 Action 소유자는
`preflight-release-repository.sh` 검사와 일치해야 합니다.

`hamn-promotion`, `hamn-validation` 환경을 만들고 관리자 우회를 끄며 `main`에서만
배포를 허용합니다. 두 환경에 secret이나 variable을 두지 않습니다. 저장소 secret은
`RELEASE_PLEASE_TOKEN`만 두며 Release Please PR에만 사용합니다.

## 물리 runner

`self-hosted`, `macOS`, `ARM64`, `hamn-validator` 라벨을 가진 온라인 macOS runner
하나를 등록합니다. Virtualization.framework를 사용할 수 있는 실제 Apple Silicon과
macOS 개발 도구, Python 3.12 이상, Docker CLI, kubectl, 명시적으로 지정한 테스트용
Kubernetes 환경이 필요합니다. 공개 PR 작업을 이 runner에 배정하지 않습니다.
릴리스 검증 job에는 릴리스 쓰기 권한이 없습니다.

`$HOME/.config/hamn/physical-validator.env`를 현재 사용자 소유의 일반 파일로 만들고
권한 0600, hard link 하나를 유지합니다. Workflow가 셸 코드로 읽으므로 검증기
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
정의합니다. 실제 객체가 있는 fixture를 수집하며 빈 값이나 작성한 예상값을 증거로
쓰지 않습니다. running fixture는 K3s를 활성화한 뒤 VM을 정지하고, stopped
fixture는 K3s 데이터를 유지하되 K3s를 비활성화한 상태입니다.

검증기는 running fixture를 고정한 구형 바이너리로 시작한 뒤 후보 TUI를 실행합니다.
stopped fixture는 후보 헤드리스 인터페이스로 시작합니다. 두 경우 모두 전환 완료,
Docker 스냅샷 보존, 테스트 VM 정리를 확인해야 합니다. Kubernetes 검증기는 고유
namespace를 만들고 제거하며 원본 kubeconfig가 동일한지 확인합니다. 독립 Kubernetes
검증기는 CNI가 없는 환경의 API 검사에 `--host-network`도 지원합니다. 전체 릴리스
검증은 클러스터 Pod 네트워크를 사용하며 독립 검사로 그 증거를 대체하지 않습니다.

## 입력과 배포 권한

`guest/image/release-inputs.json`은 Ubuntu 기반 이미지 HTTPS URL과 SHA-256을
고정합니다. 신규 이미지에는 K3s 다운로드 입력이나 호환성 서명 키가 없습니다.
GitHub artifact attestation은 산출물을 저장소·workflow·소스 commit·실행과
연결합니다. 빌드 증명은 hosted runner에서, 물리 검증 증거는 지정한 self-hosted
검증기에서 생성합니다.

설정 후 읽기 전용 검사를 실행합니다.

```sh
HAMN_RELEASE_REPOSITORY=Palbahngmiyine/Hamn \
  bash packaging/release/preflight-release-repository.sh
```

보호 정책, 두 환경, runner 라벨, secret/variable 이름, Actions 권한, 불변 릴리스를
검사합니다. GitHub 상태를 바꾸거나 secret 값을 읽지 않습니다.

후보 생성 전에 [검토 체크리스트](RELEASE-REVIEW.ko.md)를 작성하고
`make test-local-macos`를 다시 실행합니다. 로컬 계약 테스트는 물리 검증이 아닙니다.
Workflow는 다음 순서로 실행합니다.

1. Hosted Linux arm64에서 게스트 이미지를 빌드하고 attest합니다.
2. Hosted macOS arm64에서 검증하고 로컬 검사를 거쳐 정확한 후보와 hosted 증거를
   만들고 attest합니다.
3. 물리 runner에서 attestation을 검증하고 후보에서 추출한 검증기를 실행합니다.
   `physical-validation-evidence.json`을 attest합니다.
4. 배포 단계는 hosted·물리 증거를 모두 요구하고 출처·해시를 검증한 뒤 같은
   바이트를 불변 GitHub Release에 게시합니다.

`make release-gate`에는 `RELEASE_REF`, `RELEASE_TAG`, `CANDIDATE_DIR`, 빈
`OUTPUT_DIR`와 위 검증기 입력이 필요합니다. Checkout은 깨끗해야 하며 후보 소스와
같아야 합니다. RC를 다시 빌드하지 않습니다. 검증 후 소스가 바뀌면 새 후보를 만들고
검증합니다. 물리 검사가 빠지거나 산출물 바이트가 바뀌면 배포를 거부합니다.

## 설치와 호환성

공개 `install.sh`를 다운로드하고 이 저장소와 release workflow를 대상으로
`--deny-self-hosted-runners`를 지정해 GitHub attestation을 검증한 뒤 실행합니다.
설치기는 고정한 host/guest digest를 검증하고 원자적으로 게시합니다. 새 manifest를
지원하는 설치 버전은 `hamn --headless system update --yes`를 사용할 수 있습니다.
`physical-apple-silicon` manifest를 거부하는 구형 updater는 새 검증된 설치기로
업데이트해야 합니다.

이 릴리스는 CLI·JSON 호환성을 깨고 매니지드 K3s 클러스터 데이터·전용 로컬 볼륨을
자동 삭제합니다. Docker 객체·볼륨·사용자 마운트·원본 kubeconfig는 보존합니다.
바이너리 롤백으로 K3s 데이터를 복구할 수 없습니다. 공개 전에 이 변경을 릴리스
노트에 포함합니다.
