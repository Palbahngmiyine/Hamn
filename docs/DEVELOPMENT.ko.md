# 개발

영문 기준 문서는 [DEVELOPMENT.md](DEVELOPMENT.md)입니다.

Hamn은 Apple Silicon macOS 13 이상을 지원합니다. Rust는 Ratatui TUI,
헤드리스 인터페이스와 Docker/Kubernetes 클라이언트를 담당합니다. C11은 VM
수명주기·프로필·이미지·SSH·포워딩을 담당하며 Objective-C는 `host/vz/`에만
둡니다. GNU11 게스트 에이전트는 불변 Ubuntu 이미지에 포함됩니다.

## 빌드

macOS 명령줄 개발 도구와 `rust-toolchain.toml`에 고정한 Rust를 설치합니다.

```sh
make host
build/hamn --version
build/hamn --headless capabilities
```

Cargo는 `Cargo.lock`으로 의존성을 고정하고 자체 출력 디렉터리에 C/Objective-C
정적 라이브러리를 만듭니다. `scripts/build-host.py`는 게시를 직렬화하고 임시
실행 파일을 서명·검증한 뒤 `build/hamn`을 원자적으로 교체합니다. 단일 Mach-O는
macOS 시스템 라이브러리와 기존 Virtualization entitlement를 사용합니다.
Ad-hoc 서명이며 Developer ID 서명이나 공증은 아닙니다. 코어 실행 파일이나
전용 동적 라이브러리를 별도로 배포하지 않습니다.

내장 Engine API에는 Docker CLI가 필요하지 않습니다. 외부 Docker CLI·Compose·
buildx·SDK는 프로필 공개 소켓을 사용할 수 있습니다. VM에는 설치·검증된 관리형
게스트 이미지가 필요하며 서명되지 않은 클라우드 이미지로 대체하지 않습니다.
외부 Kubernetes 기능은 Hamn VM 없이 사용할 수 있습니다.

## 소스 검증

```sh
make test-control
make test-profile-state
make test-guest-deployment
make test-release-gate
make test-local-macos
```

`test-control`은 Rust 서비스, C 경계, worker 격리, 모의 Docker/Kubernetes API,
K3s 전환, PTY 터미널 복구와 바이너리 게시를 검사합니다. `test-release-gate`는
VM 없이 증거 계약을 검사합니다. 둘 다 물리 환경 릴리스 증거는 아닙니다.
`test-local-macos`는 로컬 소스·게스트·설치·업데이트·릴리스 검증을 실행하며
`actionlint`가 필요합니다. 패키징·업데이트 테스트가 공개 `build/hamn` 경로에
다른 버전을 일시적으로 빌드하므로 Make 검증은 순서대로 실행합니다.

## 게스트 이미지와 전환

`guest/image/release-inputs.json`은 Ubuntu 기반 이미지 URL과 SHA-256을 고정합니다.
`guest/image/build-ubuntu-24.04-arm64.sh`는 libguestfs가 있는 Linux arm64에서
실행합니다. `HAMN_GUEST_BASE_IMAGE`, `HAMN_GUEST_BASE_SHA256`,
`HAMN_GUEST_OUTPUT`을 지정하면 기반 이미지 digest를 검증하고 커밋된 `guest/`와
`vendor/`만 아카이브합니다. Docker·containerd·runc·CNI·binfmt·DNS·hamnd는
계속 이미지가 소유합니다. 신규 이미지에는 매니지드 K3s가 없습니다.

`host/migration/`의 고정 payload는 서명된 호스트 바이너리에 내장됩니다. K3s
제거와 기존 게스트 검증기·helper의 일회성 갱신만 허용하며 일반 소프트웨어 설치
통로가 아닙니다. 단계별 기록으로 재개하며 Docker의 `moby`와 공용 content를
보존하고 Docker 준비 상태를 확인한 뒤 완료합니다. 호스트 바이너리를 롤백해도
K3s 데이터는 복구되지 않습니다. [설정](CONFIGURATION.ko.md)을 참고하세요.

## 실행 검증

격리한 HOME, 테스트 소유 프로필, 명시적으로 선택한 테스트 Kubernetes context를
사용합니다. 기존 사용자 VM에 파괴적인 테스트를 실행하지 않습니다. 외부 Kubernetes
검증기는 고유 namespace를 만들고 `finally`에서 제거하며 kubeconfig 바이트 보존을
확인합니다.

```sh
python3 packaging/release/external-kubernetes-e2e.py --help
packaging/release/physical-e2e.sh --help
```

`make release-gate`는 정확한 후보에서 추출한 검증기를 사용합니다. 실행·정지 상태의
구형 fixture와 고정한 구형 바이너리를 준비하고 실제 Apple Silicon에서 Docker
데이터 보존을 증명해야 합니다. 입력과 배포 권한은 [릴리스 설정](RELEASE-SETUP.ko.md)을
참고하세요.

## 소스 경계

- `control/`: 타입화한 요청·결과, 공통 서비스, TUI, 헤드리스 출력.
- `host/core/`: C ABI, 프로필, VM 수명주기, 이미지·전환 조정.
- `host/vz/`: Virtualization.framework 전용.
- `host/fwd/`: 소유권을 관리하는 Docker 소켓·공개 포트 포워딩.
- `guest/agent/`, `guest/scripts/`: 게스트 관리와 이미지 소유 helper.
- `packaging/release/`: 정확한 후보 생성·검증·배포.

내부 worker는 터미널·비동기 런타임 초기화 전에 분기합니다. C 전역 상태와
fork/exit 동작은 worker에 격리합니다. TUI 종료가 별도 소유 VM supervisor를
종료하면 안 됩니다. 인터페이스 변경 시 영문·한국어 문서와 성공·실패 테스트를
함께 갱신합니다.

## Apple Silicon 작업 영역 통합 검증

결정적 TUI·게스트 검증은 `make test-control`, `make test-guest-deployment`,
로컬 릴리스 회귀 검증은 `make test-local-macos`로 실행합니다. 마지막 명령을
실행하는 동안 HEAD를 고정하세요. 아티팩트 테스트는 시작 시 소스 트리에 후보를 연결합니다.

실제 VM·Docker·Compose·buildx·폐기용 kind/Kubernetes 검증 명령입니다.

```sh
python3 tests/host/test_workspace_live.py --binary build/hamn --cache "$HOME/.hamn/cache"
```

Docker CLI·Compose/buildx 플러그인·kubectl·kind를 먼저 설치합니다. 캐시에는 선택한
서명된 게스트 이미지와 검증 마커가 필요합니다. 검증기는 `/tmp`에 소유권을 기록한
HOME을 만들고 그 환경의 명시적 Docker 소켓만 사용하며 실행 파일·이미지 해시와 결과를
저장합니다. 백업·소켓 복구, 데이터 보존, 취소·worker 강제 종료, 네이티브 PTY 명령,
Kubernetes apply·exec·port-forward를 확인합니다. kind 클러스터를 삭제하고 테스트 VM을
정지하며 HOME은 검사할 수 있게 남깁니다. `--root`는 소유한 테스트 환경만 재사용하고
`--keep-running`은 추가 진단을 위해 주 테스트 VM을 유지합니다. 증거 확인 후 해당
테스트 디렉터리를 정리하세요. 사용자 프로필을 테스트 fixture로 쓰면 안 됩니다.
