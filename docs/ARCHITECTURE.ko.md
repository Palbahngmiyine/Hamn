# 아키텍처

Hamn은 Rust 제어 계층과 정적으로 링크한 C/Objective-C 가상화 코어를 하나의 macOS
실행 파일로 제공합니다. 공개 요청은 [API](API.ko.md)를 참고하세요.

## 공통 제어 서비스

```text
TUI VM / structured requests ─┐
                             ├─ Request → service → C worker / Bollard / kube-rs
Headless operations ─────────┘
TUI Docker / Kubernetes lists ── native CLI → bounded JSON query → tables
TUI native commands ─────────── owned PTY sessions → Docker / kubectl / plugins
Docker --context (headless) ──── Bollard → private socket → Docker CLI transport
```

`control/`은 두 프런트엔드, 작업 검증, JSON 응답, API 클라이언트, 제한된 로그 큐와
취소를 담당합니다. VM·구조화된 요청은 서비스를 공유합니다. TUI 네이티브 목록과
명령은 외부 Docker·kubectl을 쓰며, 목록은 제한된 수명의 JSON 조회, 대화형 명령은
소유권을 가진 PTY 세션으로 실행합니다. Kubernetes 선택 변경은 guarded kubectl
경로에서 UID·resourceVersion을 검사합니다. 화면 전환 시 이전
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
IPv4 TCP·UDP Ports를 한 번의 컨테이너 목록 응답에서 검증한 뒤 완전한 snapshot만
반영합니다. 컨테이너마다 inspect를 반복하지 않습니다. 외부 Docker CLI·Compose·buildx·SDK·Testcontainers도
같은 소켓을 사용합니다. Hamn은 외부 도구의 현재 context를 바꾸지 않습니다.
레지스트리 자격 증명은 외부 클라이언트가 관리하고 홈 공유는 자격 증명 격리 경계가 아닙니다.

헤드리스 Kubernetes는 kube-rs, TUI 네이티브 탐색은 kubectl로 선택한 외부 context에
접속합니다. Hamn VM, 게스트 CRI,
Hamn API 포워딩은 사용하지 않습니다. kubeconfig·자격 증명은 로컬에서 읽고 원본
파일은 바꾸지 않습니다. 변경은 객체 식별자·resourceVersion 사전 조건으로 보호하고,
변경 요청이 반복되지 않도록 자동 HTTP 재시도를 끕니다.

## 게스트 설정과 복구

서명된 Ubuntu 24.04 arm64 이미지가 hamnd, Docker, 공용 containerd, runc, CNI,
binfmt와 일반 게스트 helper를 소유합니다. 서명 없는 이미지 대체 경로나 시작 시
게스트 코드를 빌드하기 위한 호스트 소스 마운트는 없습니다.

새 프로필 디스크는 `host/image/raw_cache.c`를 사용합니다. 이미지 digest별 캐시
묶음에 sparse raw base와 extractor 버전·가상 크기·SHA-256 marker를 저장합니다.
digest별 lock 대기는 60초로 제한하며, 비공개 stage와 파일·디렉터리 fsync 후
원자적으로 게시합니다. 재사용 시 소유자, 권한, hard link 수, 크기와 이미지·raw
hash를 검증합니다. 중단된 stage는 같은 lock 아래에서 복구합니다. 이 캐시는
네트워크 요청을 하지 않습니다.

APFS에서는 파일 descriptor를 받는 `clonefile(2)` 계열의 `fclonefileat`로 복제한 뒤
해당 디스크를 요청 크기로 확장합니다. `EXDEV`, `ENOTSUP`, `EOPNOTSUPP`에만
검증된 이미지 descriptor의 sparse extraction으로 전환합니다. 권한·무결성·I/O
오류는 실패로 처리합니다. 기존 디스크는 rebase하거나 교체하지 않으며, 설정 크기를
명시적으로 늘린 경우에만 기존 inode를 확장합니다. 전체 hash 검증에는 최초 생성과
재사용 모두 I/O가 필요하므로 저장 공간 공유가 항상 더 빠른 생성을 뜻하지는 않습니다.

일반 게스트 트랜잭션은 변경 전 런타임 설정·서비스 상태를 저장합니다. commit과
Docker·containerd 준비 확인 후 배포 fingerprint를 기록합니다. 중단된 트랜잭션이
남긴 백업은 다음 배포 전, 그리고 실행 중인 VM을 준비 완료로 보고하기 전에
처리합니다. 호스트는 게스트 배포 잠금 아래에서 고정된 복구 스크립트
(`host/core/deployment_recovery.h`)를 보내며, 이 스크립트는 완전하고 소유권이 맞는
백업 하나만 이미지의 트랜잭션 helper로 rollback하고, 불완전하거나 모호하거나
안전하지 않은 항목은 복구하지 않고 점검하도록 보고합니다. 이미 설치된 이미지에는
자체 복구 동작이 없으므로 스크립트를 호스트에서 보내지만, 게스트에 아무것도
설치하지 않습니다.

## 단일 실행 파일 빌드

`make host`는 C/Objective-C 정적 아카이브와 Rust를 링크하고, 임시 후보 파일을 서명·검사한
뒤 `build/hamn`에 원자적으로 게시합니다. Cargo.lock과 rust-toolchain.toml로 의존성과
컴파일러를 고정합니다. macOS 시스템 라이브러리, SSH, 게스트 이미지·실행 파일,
kubeconfig 인증 플러그인은 허용합니다. 별도 호스트 코어나 전용 동적 라이브러리는
실행 시 필요하지 않습니다.

## 릴리스 설치 지원

`control/install_support/`는 같은 실행 파일의 내부 `__install-support` 모드를
구현하며 Tokio·터미널 초기화 전에 실행합니다. 아카이브·manifest 검증, stable 버전
판정, manifest-only 검사, artifact 획득과 byte 집계, 설치 기록 호환, 그리고 설치·
업데이트 거래 전체를 담당합니다. 고정된 순서의 거래·캐시·설치 잠금(`locks.rs`),
version-3 복구 저널과 롤백(`journal.rs`), generation staging, 검증된 Hamn 0.1.x
generation의 이전과 rename 한 번으로 하는 명령 링크 게시(`generation.rs`), signal
checkpoint(`interrupt.rs`), 업데이트 순서(`update.rs`),
불필요한 설치본 정리(`retention.rs`)가 여기에 있습니다. 한 프로세스가 거래의 모든
잠금을 보유하고 릴리스의 `bin/hamn`을 직접 설치하므로, 호스트 아카이브는 generation
payload(`bin/hamn`과 `share/hamn/update-manifest-url`)일 뿐이며 설치를 위해 그 안의
어떤 것도 실행하지 않습니다. `hamn upgrade`는 core worker(`host/cmd/cmd_update.c`)를
거쳐 업데이트기에 도달하며, worker는 자신의 버전과 generation을 전달해 대기 중에
generation이 바뀌면 업데이트기가 거부하게 합니다. `make install`과 최초 설치기는
내부 모드를 직접 호출합니다. 호스트 실행 파일을 검증하기 전에는 macOS 기본
`zsh/system`이 공통 digest 잠금과 크기를 제한한 partial 기록을 소유합니다.
고정 크기·SHA-256을 검증한 다음 `tar` 표준 출력으로 실행 파일을 읽으며, native
updater를 호출하기 전에 다운로드 프로세스와 잠금을 정리합니다. 이후 manifest·설치
기록·전송 판단은 Hamn 내부에서 실행하며 설치는 Python이나 shell 스크립트를
실행하지 않습니다.
조회 프로세스 그룹에는 실행 시간 제한을 적용하며, 업데이트 복구와 정리가
끝날 때까지 두 설치 경로의 잠금을 유지합니다. 보존·호환 범위는
[설치 문서](INSTALLATION.ko.md)를 참고하세요.

## Mount 및 network 경계

`$HOME`은 기본 virtiofs share이며 비활성 또는 read-only로 바꿀 수 있습니다. Custom
host path는 VM 시작 전에 canonicalize합니다. Absolute path, symlink traversal 없는
사용자 소유 directory여야 합니다. Writable custom path는 `$HOME` 아래에 있어야 하고,
그 밖의 path는 기본 read-only입니다.

모든 Hamn profile은 Virtualization.framework shared NAT를 사용합니다. Published TCP
port는 SSH ControlMaster forward를, published UDP port는 bounded host relay를 사용합니다.
Forward 생성/제거는 transactionally reconcile합니다. Network attachment는 profile마다
설정할 수 없습니다. `network` YAML key와 network 선택 CLI option이 없습니다.
`host.docker.internal`은 guest Docker network에 제공됩니다. 0.0.1의
`host.hamn.internal` alias는 제거되었습니다.

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
