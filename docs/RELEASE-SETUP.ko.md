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

## 릴리스 도구

`packaging/release`의 릴리스 스크립트는 셸 조율만 맡고 JSON·증거·버전·GitHub
검사는 `hamn-dev release`(`tools/hamn-dev`, 배포하지 않음)에 맡깁니다. Make
target이 이를 빌드하여 절대 경로를 `HAMN_DEV`로 전달하고, 릴리스 workflow는
`rust-toolchain.toml`에 고정한 toolchain으로 `cargo build --locked -p hamn-dev`를
실행해 빌드합니다. 하위 명령 없이 `hamn-dev release`를 실행하면 하위 명령과
인자 목록을 출력합니다.

## 선택적 수동 물리 검증

실제 VM·Docker·Kubernetes 검증은 `make release-gate`로 별도 실행할 수 있으며
자동 배포의 필수 조건은 아닙니다. 격리된 Apple Silicon 장비에 macOS 개발 도구,
`rust-toolchain.toml`에 고정한 Rust toolchain, Docker CLI, kubectl과 테스트용
Kubernetes context가 필요합니다. 이 선택적 로컬 검사를 위해 저장소 runner를
등록하지 않습니다.

`$HOME/.config/hamn/physical-validator.env`를 현재 사용자 소유의 일반 파일로 만들고
권한 0600, hard link 하나를 유지합니다. 수동 로컬 검증에서만 셸 코드로 읽으며 검증기
관리자만 편집할 수 있어야 합니다. 다음 값을 지정합니다.

```sh
HAMN_E2E_CONTEXT='dedicated-test-context'
HAMN_E2E_KUBECONFIG='/absolute/path/test-kubeconfig'
```

`make release-gate`는 checkout에서 `hamn-dev`를 빌드하고
`hamn-dev release physical-e2e`를 실행합니다. 검증기는 정확한 후보 host archive의
모든 항목을 검사한 뒤 풀고, archive 안 실행 파일의 서명·의존성·버전을 확인한 다음
`/private/tmp` 아래 비공개 임시 HOME에서 실행합니다. 사용자 VM이나 kubeconfig는
사용하지 않습니다. 후보 게스트 이미지로 격리 프로필 두 개를 만들어 시작하고,
구조화된 Docker API와 외부 Docker socket을 사용하며, PTY에서 TUI를 열고 종료하고
(터미널 설정이 복원되고 VM이 계속 실행되어야 합니다), VM을 정지·재시작한 뒤
외부 Kubernetes 검증기를 실행합니다. Kubernetes 검증기는 고유 namespace를 만들고
제거하며 원본 kubeconfig가 동일한지 확인합니다. 소유한 모든 프로필이 정지된 것을
확인한 뒤에만 작업 디렉터리를 제거하며, 정지에 실패하면 작업 디렉터리를 남깁니다.

물리 증거는 schema 3(`physical-validation-evidence.json`)입니다. 매니지드 K3s
전환을 제거하면서 구형 K3s 전환 검사·fixture와 schema 2의 `legacy` 항목도
제거했으며 schema 2 증거는 거부합니다. 기본값은 클러스터 Pod 네트워크입니다.
CNI가 동작하지 않는 API 검증 클러스터에서는 `HAMN_E2E_K8S_HOST_NETWORK=1`을 명시할
수 있습니다. 증거에 `podNetwork: "host"`가 기록되며 Kubernetes 조작과 로그를
검증하지만 Pod 네트워크 연결을 입증하지는 않습니다. 독립 Kubernetes 검증기인
`hamn-dev release external-kubernetes-e2e`에는 같은 의미의 `--host-network`
옵션이 있습니다.

## 입력과 배포 권한

`guest/image/release-inputs.json`은 Ubuntu 기반 이미지 HTTPS URL과 SHA-256을
고정합니다. 신규 이미지에는 K3s 다운로드 입력이나 호환성 서명 키가 없습니다.
GitHub artifact attestation은 산출물을 저장소·workflow·소스 commit·실행과
연결합니다. 자동 배포 증명은 hosted runner에서 생성합니다. Manifest에는
`validationMode: github-hosted-no-vm`, hosted 증거에는 `physicalE2E: false`를
기록하며 실제 VM E2E를 검증했다고 주장하지 않습니다.
게시 단계는 schema v3 manifest `hamn-update-manifest-v3.json`만 생성하고,
digest를 검증한 후보 archive의 실행 파일, 즉 설치된 client와 같은 parser로
검증합니다. schema v2 `hamn-update-manifest.json`은 더 이상 게시하지 않습니다.
이를 읽는 Hamn v0.1.2 이하는 업데이트 확인 시 HTTP 404를 받으므로 공개된
`install.sh`로 다시 설치해야 합니다.

설정 후 읽기 전용 검사를 실행합니다.

```sh
make hamn-dev
HAMN_DEV="$PWD/target/release/hamn-dev" \
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

임시 Linux builder에서 선택적 `passt`를 제거해 libguestfs가 QEMU SLIRP
네트워크를 일관되게 사용하도록 합니다. 이미지 조립은 패키지 설치 전에 30초 제한으로
DNS를 검사하며 runner 이미지의 네트워크 변경으로 발생한 실패를 숨기지 않습니다.

### 게스트 이미지 크기 증거

신뢰된 Linux arm64 builder에는 `build-essential`과 `zlib1g-dev`도 필요합니다.
호스트에 포함하는 것과 같은 qcow2 decoder를 빌드하고, 추출한 raw SHA-256을
`qemu-img` 결과와 비교합니다. `guest/image/build-ubuntu-24.04-arm64.sh`는 build
package와 재생성 가능한 내용을 제거하기 전, 동일하게 준비된 파일시스템에서 압축
baseline을 생성합니다. 따라서 전후 측정은 한 번 provision한 package 집합에서
시작하며, 별도 빌드 사이의 저장소 package 버전까지 고정한다는 뜻은 아닙니다.
runtime package를 보호한 상태에서 gcc/make와 불필요한 build 의존성, apt·로그·임시
파일을 제거하고 최초 부팅 상태를 초기화한 뒤 free block을 discard합니다. 압축에는
zlib를 사용하고 가상 디스크는 8 GiB로 유지합니다. 바이트 동등성은 정리한 stage와
그 압축 결과 사이에서 확인합니다. package를 제거하기 전 baseline과는 내용이 다릅니다.

builder는 최소 `max(64 MiB, 5%)` 감소, 2 GiB 미만의 압축 크기, decoder와 reference
일치, protective MBR와 GPT CRC 검사를 요구합니다. `<image>.size-report.json`과
`<image>.packages-before.tsv`, `<image>.packages-after.tsv`를 기록합니다.
report는 실제 크기·해시를 기반 이미지 digest와 소스 revision에 연결하며, 이미지와
함께 release evidence와 attestation에 포함합니다.

측정 전용 소스 snapshot은 해당 snapshot의 근거이며, 이후 PR revision이나 최종
release candidate의 검증을 대신하지 않습니다. 게시 단계는 정확한 이미지 크기·digest와
함께 report의 `sourceRevision`이 후보 commit과 같은지 확인합니다. 릴리스 소스나
budget이 바뀌면 최종 commit에서 배포 산출물을 다시 빌드하고 검증합니다.
이전 report의 revision을 바꾸거나 다른 이미지·호스트 byte의 런타임 증거를 재사용하지 않습니다.

첫 실측 결과를 검토한 뒤 `guest/image/release-size-budget.json`을 만들어야 합니다.
격리된 builder에서 일반 base image/hash/output 입력과 함께
`HAMN_GUEST_SIZE_REVIEW_ONLY=1`을 지정하면 검토 후보와
`<image>.size-report.budget-proposal.json`을 생성합니다. 이 모드도 최소 감소량과
구조 검사를 강제하지만 `reviewOnly: true` report로는 게시할 수 없습니다.
실제 footprint와 런타임 증거를 검토한 후 승인한 proposal을 budget으로 commit하고
일반 후보를 빌드합니다. budget 상향에는 새 footprint 검토가 필요하며, budget이
없으면 실패합니다. 게시 단계는 `make -C guest image-tool`로 빌드한 C 도구
`guest/build/hamn-image-tool verify-release-size IMAGE SIZE_REPORT REVIEWED_BUDGET COMMIT`으로
정확한 이미지와 report, report의 소스 revision을 다시 검증합니다.

실제 before/after 런타임 검증에는 같은 빌드에서
`HAMN_GUEST_BASELINE_OUTPUT=/owned/output/hamn-baseline.img`도 지정합니다.
이 선택 옵션은 정리 전 provisioned 이미지와 `.sha256` 파일을 mode 0600으로
보존합니다. 크기 report의 baseline과 대조하고 기존 이미지 gate를 모두 통과한
뒤에만 완성된 파일을 공개합니다. 기존 파일, symlink, 다른 사용자에게 쓰기를 허용한 디렉터리,
후보 산출물과 겹치는 경로를 거부합니다. baseline은 검토 증거이며 배포 자산이 아닙니다.

Linux arm64 builder에서 재현 가능한 정리 변형 두 개를 생성하려면 다음을 실행합니다.

```sh
make -C guest image-tool
guest/build/hamn-image-tool variations --source-root . \
  --baseline /owned/output/hamn-baseline.img \
  --size-report /owned/output/hamn-guest.img.size-report.json \
  --output-directory /owned/output/variations --seed 20260921
```

출력 디렉터리는 없어야 합니다. fixture는 삭제 가능한 로그, apt 캐시, 임시 파일,
첫 부팅 식별자를 변형하고 정리·패키지 목록 비교·압축·decoder/reference·GPT·크기
gate를 다시 실행합니다. `variations.json`에는 생성 입력별 이미지 digest와 물리
런타임 검증 대기 상태를 기록합니다. baseline, 일반 후보, 각 변형 이미지를 각각
소유가 분명한 일회용 profile에서 부팅하여 기능 동등성을 검증해야 하며, 구조 검사
통과만으로 해당 검증을 완료한 것으로 처리하지 않습니다.

Hosted 구조 검사는 부팅이나 기능 동등성의 증거가 아닙니다. 자동 게시 workflow가
선택적 수동 gate를 실행하지 않더라도 이 이미지 최적화의 수용에는 물리 검증이 필요합니다.
최적화한 정확한 산출물로
Docker API·CLI, Compose, Buildx, containerd/runc/CNI, amd64 binfmt, opt-in Rosetta,
외부 Kubernetes 연결과 재부팅 후 데이터 보존을 별도 확인해야 합니다. 합성 크기 fixture나 sparse 디스크의
로컬 테스트는 이미지 크기 감소나 VM 동작을 입증하지 않습니다. Linux 빌드나 물리
검증을 실행하지 못했을 때 임의의 baseline·budget으로 대신하지 않습니다.

`make release-gate`에는 `RELEASE_REF`, `RELEASE_TAG`, `CANDIDATE_DIR`, 빈
`OUTPUT_DIR`와 위 검증기 입력이 필요합니다. Checkout은 깨끗해야 하며 후보 소스와
같아야 하고, 검증기는 이 checkout에서 빌드합니다. RC를 다시 빌드하지 않습니다. 검증 후 소스가 바뀌면 새 후보를 만들고
검증합니다. 물리 검사가 빠지면 수동 gate가 실패합니다. 산출물 바이트가 바뀌면
hosted 검사가 통과했더라도 자동 배포를 거부합니다.

## 설치와 호환성

공개 `install.sh`를 다운로드하고 이 저장소와 release workflow를 대상으로
`--deny-self-hosted-runners`를 지정해 GitHub attestation을 검증한 뒤 실행합니다.
설치기는 고정한 host/guest digest를 검증하고 원자적으로 게시합니다. v3 manifest를
읽는 설치 버전은 `hamn --headless system update --yes`를 사용할 수 있으며, v0.1.2
이하는 `install.sh`로 다시 설치해야 합니다. Hosted manifest는 0.0.1 릴리스에서
사용한 `github-hosted-no-vm` 검증 모드를 유지합니다.

매니지드 K3s 프로필은 더 이상 전환하지 않습니다. 매니지드 K3s의 `kubernetes:`
키가 남은 프로필은 거부합니다. 릴리스 노트에는 더 이상 K3s 데이터 삭제 경고를
넣지 않습니다.
