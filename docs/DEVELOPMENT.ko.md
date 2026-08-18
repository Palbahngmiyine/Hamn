# 개발

이 문서는 표준 한국어 개발 가이드입니다. 영문 기준 문서는
[DEVELOPMENT.md](DEVELOPMENT.md)입니다.

Hamn 0.0.1은 Apple Silicon macOS 13 이상용 Docker-only local container
제품입니다. Host 코드는 C11이며 Virtualization.framework를 사용합니다.
Objective-C 코드는 `host/vz/`에만 둡니다. Guest 코드는 GNU11이며 immutable Ubuntu
guest image에 빌드되어 들어가므로, 실행 중인 VM에 host checkout을 복사하지 않습니다.

개발할 Desktop, XPC, Docker shim, `hamn docker`, `nerdctl`, public containerd
socket, external Kubernetes catalog, Homebrew Cask, DMG build 경로는 없습니다.

## Build

macOS command-line developer tools가 설치된 Apple Silicon Mac에서 실행합니다.

```sh
make host
build/hamn version
```

`make host`는 C11 경고를 켠 `build/hamn`을 만들고 Virtualization entitlement만
포함해 ad-hoc sign합니다. 이는 Developer ID 또는 notarized distribution build가
아닙니다.

실제 Docker CLI는 별도로 설치하세요. Hamn은 Docker CLI를 설치하거나 대체하지
않습니다. Source binary에는 선택된 guest image가 없으므로 signed release가 image를
install/update하기 전 `hamn start`는 의도적으로 실패합니다.

## Source gate

여러 target이 같은 `build/hamn`을 다시 만들므로 Make target은 한 번에 하나씩
실행합니다.

```sh
make test-portable
make test-core-quality
make test-profile-state
make test-guest-deployment
make test-diagnostics
make test-install
make test-uninstall
make test-update
make test-kubernetes-cli
make test-release-artifacts
make test-hosted-validation
make test-release-publish
```

`make test-local-macos`는 적용 가능한 host/source gate를 직렬로 실행하며
`actionlint`도 필요합니다.

`make test-release-gate`는 maintainer 실험용 선택적 physical VM E2E harness로
남아 있습니다. `test-local-macos` 또는 자동 release authority에는 포함되지 않습니다.

Shared host binary를 다시 만드는 source test를 동시에 실행하지 마세요.

## Guest image 작업

Release guest image는 Ubuntu 24.04 arm64입니다. `hamnd`, Moby `dockerd`,
system containerd/CRI, runc, BuildKit, CNI, binfmt, guest configuration helper,
K3s manifest/trust material이 이미 포함되어야 합니다. Image builder는 trusted Linux
arm64 환경과 signed input metadata가 필요합니다.

```sh
bash guest/image/build-ubuntu-24.04-arm64.sh --help
```

Builder는 signature/checksum이 없으면 실패합니다. Git checkout에서만 실행해야 하며,
Image를 준비하는 동안 committed `guest/`와 `vendor/` tree만 archive합니다. untracked
checkout file은 image input이 될 수 없습니다. 그 source는 완성된 root image에서
제거합니다. VM boot 시 host는 profile configuration 및 허용된 virtiofs mount만
제공합니다. Checkout의 `/opt/hamn`을 mount하거나 guest code를 build하지 않습니다.

## Local runtime 원칙

Destructive/lifecycle test에는 isolate한 `HOME`을 사용하세요. Source checkout을
기존 `~/.hamn`, Docker context, Colima profile에 연결하지 마세요.

Interface를 변경할 때는 canonical English/Korean Markdown을 모두 갱신하고,
deterministic success/failure test를 추가한 뒤 작은 관련 gate부터 실행합니다. Host,
guest, vendored code의 소유 경계를 섞지 않습니다.

## 확인할 주요 경계

- `host/cmd/`: CLI parsing과 profile lifecycle
- `host/vz/`: 유일한 Virtualization.framework implementation
- `host/fwd/`: Docker publish TCP/UDP forwarding. Docker API client가 아님
- `guest/agent/`: guest control agent. Container engine이 아님
- `guest/scripts/`: image-provided Docker/containerd/K3s/Rosetta의 transactional
  configuration
- `packaging/release/`: release byte build/validation/promotion

Architecture 경계는 [Architecture](ARCHITECTURE.ko.md), 전체 profile schema는
[Configuration](CONFIGURATION.ko.md)를 보세요.
