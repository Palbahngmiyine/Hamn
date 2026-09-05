# 아키텍처

Hamn은 Rust 제어 계층과 정적으로 링크한 C/Objective-C 가상화 코어를 하나의 macOS
실행 파일로 제공합니다. 공개 요청은 [API](API.ko.md)를 참고하세요.

## 공통 제어 서비스

```text
Ratatui/Crossterm TUI ─┐
                      ├─ 타입화된 요청 → 공통 서비스 → 결과 / 이벤트
Clap 헤드리스 CLI ────┘                       │
                         ┌───────────────────┼───────────────────┐
                         ▼                   ▼                   ▼
                    C worker             Bollard             kube-rs
                   같은 실행 파일      Docker Engine     외부 Kubernetes
                         │               Unix 소켓           kubeconfig
                         ▼
                   프로필 VM 수명주기
                Virtualization.framework
```

`control/`은 두 프런트엔드, 작업 검증, JSON 응답, API 클라이언트, 제한된 로그 큐와
취소를 담당합니다. TUI와 헤드리스는 같은 서비스를 사용합니다. 화면 전환 시 이전
대상의 늦은 응답을 버리고, 네트워크 요청은 비동기로 실행합니다.

`host/core/control.h`의 C 함수는 새 `__core-worker` 프로세스에서만 호출합니다.
진입점은 Tokio·터미널 초기화 전에 내부 C 모드로 분기합니다. C 전역 상태·fork·exit가
Rust 다중 스레드 프로세스에 영향을 주지 않도록 분리한 구조입니다. worker JSON은
C 상태에서 직접 생성하며 기존 CLI 텍스트를 파싱하지 않습니다.
반환 메모리는 호출자가 `hamn_control_free`로 해제합니다.

## VM과 소켓 소유권

C는 프로필 설정, VM 식별 검증, 수명주기·변경 잠금, SSH ControlMaster, 포트 관찰기,
이미지 검증, 프로필 상태를 소유합니다. Objective-C 구현은 `host/vz/`에 유지합니다.

프로필은 `~/.hamn/<profile>/` 아래의 디스크, SSH 키, vmrun 식별 기록, `vmrun.sock`,
`ssh.sock`, `docker.sock`, `agent.sock`을 소유합니다. VM 소유자는 같은 실행 파일의
별도 프로세스입니다. TUI 종료는 프런트엔드 작업을 취소하지만 VM을 정지하지 않습니다.
C supervisor는 worker가 사라져도 잠금을 유지하며 하위 프로세스를 정리합니다.

Docker API는 SSH로 전달한 프로필 Unix 소켓을 통해 게스트 dockerd에 도달합니다.
Docker는 공용 containerd의 `moby` 네임스페이스를 사용하며, C 포트 관찰기는 공개
TCP·UDP 포트를 계속 조정합니다. 외부 Docker CLI·Compose·buildx·SDK·Testcontainers도
같은 소켓을 사용합니다. Hamn은 외부 도구의 현재 context를 바꾸지 않습니다.
레지스트리 자격 증명은 외부 클라이언트가 관리하고 홈 공유는 자격 증명 격리 경계가 아닙니다.

Kubernetes는 선택한 외부 context에 kube-rs로 접속합니다. Hamn VM, 게스트 CRI,
Hamn API 포워딩은 사용하지 않습니다. kubeconfig·자격 증명은 로컬에서 읽고 원본
파일은 바꾸지 않습니다. 변경은 객체 식별자·resourceVersion 사전 조건으로 보호하고,
변경 요청이 반복되지 않도록 자동 HTTP 재시도를 끕니다.

## 게스트 설정과 구형 설치 전환

서명된 Ubuntu 24.04 arm64 이미지가 hamnd, Docker, 공용 containerd, runc, CNI,
binfmt와 일반 게스트 helper를 소유합니다. 서명 없는 이미지 대체 경로나 시작 시
게스트 코드를 빌드하기 위한 호스트 소스 마운트는 없습니다.

구형 프로필은 SSH 준비 직후 일반 provisioning보다 먼저 K3s 전환을 시작합니다.
기존 EFI 부팅을 유지하므로 SSH 준비 전에는 구 K3s가 잠시 실행될 수 있습니다.
고정 Python payload와 새 검증기·트랜잭션 helper는 서명된 호스트에 내장합니다.
이는 기존 디스크를 위한 한정된 일회성 교체이며 호스트 체크아웃을 일반 게스트
설정의 원본으로 사용하지 않습니다.

게스트의 root 소유 기록은 소유권 검증, 서비스 정지·mask, `k8s.io` 리소스 정리,
전용 파일 삭제, helper 갱신, Docker 준비 확인 단계를 저장합니다. 중단된 단계는
재시도합니다. 공용 content, Docker 객체, 사용자 마운트, 원본 kubeconfig는 보존합니다.
K3s 데이터 삭제는 이전 바이너리로 복구되지 않습니다. C는 게스트 전환과 프로필의
API 포워딩 정리가 성공한 뒤 새 프로필 형식을 게시합니다.

일반 게스트 트랜잭션은 변경 전 런타임 설정·서비스 상태를 저장하고 복구합니다.
commit과 Docker·containerd 준비 확인 후 배포 fingerprint를 기록합니다.
K3s 전환은 이 rollback과 분리되어 런타임 설정 복원이 K3s 데이터를 되살리지 않습니다.

## 단일 실행 파일 빌드

`make host`는 C/Objective-C 정적 아카이브와 Rust를 링크하고, 임시 후보 파일을 서명·검사한
뒤 `build/hamn`에 원자적으로 게시합니다. Cargo.lock과 rust-toolchain.toml로 의존성과
컴파일러를 고정합니다. macOS 시스템 라이브러리, SSH, 게스트 이미지·실행 파일,
kubeconfig 인증 플러그인은 허용합니다. 별도 호스트 코어나 전용 동적 라이브러리는
실행 시 필요하지 않습니다.

## Mount 및 network 경계

`$HOME`은 기본 virtiofs share이며 비활성 또는 read-only로 바꿀 수 있습니다. Custom
host path는 VM 시작 전에 canonicalize합니다. Absolute path, symlink traversal 없는
사용자 소유 directory여야 합니다. Writable custom path는 `$HOME` 아래에 있어야 하고,
그 밖의 path는 기본 read-only입니다.

모든 Hamn profile은 Virtualization.framework shared NAT를 사용합니다. Published TCP
port는 SSH ControlMaster forward를, published UDP port는 bounded host relay를 사용합니다.
Forward 생성/제거는 transactionally reconcile합니다. Network attachment는 profile마다
설정할 수 없습니다. `network` YAML key와 network 선택 CLI option이 없습니다.
`host.docker.internal`은 guest Docker network에 제공되며 `host.hamn.internal`은 0.0.1
compatibility alias입니다. Guest Docker configuration은 다음 release 제거 전에 경고를
출력합니다.

## 호환성 경계

Guest는 amd64 Linux image에 기본으로 `binfmt`를 사용합니다. Rosetta는 host가 지원할 때
Virtualization framework의 Linux Rosetta directory share를 이용하는 opt-in 기능입니다.
Nested virtualization도 opt-in입니다. macOS 15 이상에서 Hamn은 Apple의
[nested virtualization capability check](https://developer.apple.com/documentation/virtualization/vzgenericplatformconfiguration/isnestedvirtualizationsupported)를
확인한 뒤에만 이를 켭니다. Apple은 이 capability를 M3 칩 이상 Mac에서 사용할 수 있다고
문서화합니다.

이 release에는 Intel Mac backend, Linux host backend, Incus runtime, GPU/AI integration,
managed kind cluster, public containerd socket, Desktop app,
XPC service, Homebrew Cask, DMG, notarization, Docker shim이 없습니다.
