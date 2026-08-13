# 아키텍처

이 문서는 Hamn 0.0.1 아키텍처의 한국어 번역입니다. 기준 영문 문서는
[ARCHITECTURE.md](ARCHITECTURE.md)입니다.

## 설계 경계

Hamn은 프로필별 Linux VM과 host ↔ guest 사이의 좁은 전송 계층을 소유하는 macOS
CLI입니다. Container engine, Docker CLI 대체품, Desktop application, host containerd
distribution이 아닙니다.

macOS XNU에는 Linux container process가 요구하는 Linux kernel ABI가 없습니다. 여기에는
Linux namespace, cgroup, overlay filesystem이 포함됩니다. 따라서 제품 경계는 macOS에서
Linux container를 직접 실행하는 것이 아니라 Apple Virtualization.framework로 만드는 Linux
guest VM입니다. Apple의 [Creating and Running a Linux Virtual
Machine](https://developer.apple.com/documentation/virtualization/creating-and-running-a-linux-virtual-machine)은
이를 위해 architecture에 맞는 Linux image와 VM device configuration이 필요함을 설명합니다.

```text
macOS, profile <name>                         Ubuntu 24.04 arm64 guest
────────────────────────────────────────────  ─────────────────────────────────
hamn CLI                                       Linux kernel
  lifecycle lock                               hamnd (작은 guest-control agent)
  VZ VM owner                                  dockerd
  SSH ControlMaster                             system containerd + CRI plugin
  Docker socket forward                         runc, CNI, BuildKit, binfmt
  port observer / TCP forward / UDP relay       선택형 K3s + kubelet
  virtiofs mount 설정
```

Guest는 release가 선택한 preconfigured image입니다. Runtime binary,
`hamnd.service`, guest helper script, K3s compatibility manifest, public key를
포함합니다. Boot 시 Hamn은 Docker daemon JSON, Rosetta 선택처럼 profile이 제어하는
configuration만 reconcile합니다. 실행 중인 VM에 source code를 rsync하거나 첫 boot에
guest runtime을 compile하지 않습니다.

## Process 및 socket 소유권

프로필 하나는 private state directory 하나를 소유합니다.

```text
~/.hamn/<profile>/
  config.yaml               strict profile configuration
  disk.img                  guest data disk
  docker.sock (0600)        forward된 guest Docker Engine API
  agent.sock                forward된 Hamn guest-agent control socket
  state.json, VM PID, lock, log, SSH control path, port record
  kubeconfig                K3s를 명시적으로 활성화한 경우에만 생성
```

기본 프로필 Docker context는 `hamn`, 이름 있는 프로필은 `hamn-<profile>`입니다.
프로필별 Docker socket만이 Hamn이 host에 노출하는 container API입니다. SSH를 통해
guest 내부 `/var/run/docker.sock`으로 forward합니다. Host `/var/run/docker.sock`은
Hamn이 만들거나 교체하거나 사용하지 않습니다.

System containerd socket `/run/containerd/containerd.sock`은 guest 내부에만 남습니다.
이는 native containerd endpoint이며 Docker Engine endpoint나 지원되는 host API가
아닙니다. `status --json`은 이 socket을 노출하는 대신 Docker API readiness와 CRI
readiness를 분리해 출력합니다.

## Docker 경로

```text
macOS Docker CLI / Compose / buildx / SDK
  │  Docker Engine API
  ▼
~/.hamn/<profile>/docker.sock  (0600, SSH Unix-socket forward)
  ▼
guest /var/run/docker.sock
  ▼
guest dockerd --containerd=/run/containerd/containerd.sock
  │  native containerd API, namespace moby
  ▼
guest system containerd
  ▼
runc -> Linux kernel
```

Docker는 [dockerd 문서](https://docs.docker.com/reference/cli/dockerd/)에서 별도로
시작한 containerd를 `--containerd`로 지정할 수 있고 기본 containerd namespace가
`moby`라고 설명합니다. Docker의 logical state는 Docker가(`/var/lib/docker`와
`moby`) 소유하고, containerd native store는 guest system service가 소유합니다.

Hamn은 임의의 Docker request를 자체 proxy하지 않습니다. Internal Docker observer는
published port 동기화에 한정됩니다. Docker event와 inspect data를 읽고 자신이 만든
macOS forwarding resource만 소유합니다.

Registry credential은 host Docker client의 책임으로 남습니다. Host Docker CLI/SDK는
구성된 credential helper를 해석하고 profile socket을 거쳐 individual Docker API request에
registry authorization을 넣습니다. Hamn은 `docker login`을 실행하거나 credential helper를
복사하거나 guest `/home/hamn/.docker` credential store를 만들지 않습니다. 기본 home
virtiofs share는 사용자가 노출한 filesystem이지 credential isolation boundary가 아니므로
그렇게 취급하면 안 됩니다.

## Kubernetes 경로

```text
macOS kubectl
  │  HTTPS Kubernetes API, profile-local kubeconfig
  ▼
SSH loopback forward -> guest K3s API server
  ▼
guest kubelet
  │  CRI gRPC
  ▼
guest system containerd, namespace k8s.io
  ▼
runc -> Linux kernel
```

CRI는 kubelet과 runtime 사이의 protocol이며 Docker containerd API가 아닙니다.
[Kubernetes CRI 문서](https://kubernetes.io/docs/concepts/containers/cri/)가 이
경계를 정의합니다. Host `kubectl`은 CRI 또는 containerd socket에 직접 연결하지
않습니다.

새 프로필의 K3s는 비활성입니다. 사용자가 `hamn kubernetes start`를 실행하면 guest가
signed compatibility manifest와 checksum을 검증하고, 고정된 K3s artifact를 설치한 뒤
기존 system containerd를 사용하도록 설정합니다. 이어서 node와 CoreDNS readiness를
기다립니다. K3s는 `k8s.io` namespace와 K3s state directory를 소유합니다. Docker의
`moby` namespace는 분리됩니다.

프로필 전용 kubeconfig context는 기본 프로필에서 `hamn`, 다른 프로필에서
`hamn-<profile>`입니다. Foreign context와 충돌하면 실패하며 덮어쓰지 않습니다.
`hamn kubectl`은 `--kubeconfig` override를 거부하므로 다른 cluster에 조용히 동작할 수
없습니다.

containerd는 native CLI/API와 CRI를 구분하고 CRI plugin이 containerd에 내장됨을
문서화합니다. [getting
started](https://github.com/containerd/containerd/blob/main/docs/getting-started.md)를
참조하세요.

## Lifecycle 및 rollback

`start`는 profile mutation을 직렬화하고 immutable image input, disk, SSH key,
cloud-init seed, mount, VM state를 준비한 뒤 VZ VM owner를 시작합니다. Shared NAT는
DHCP lease로 guest address를 찾습니다. Hamn에는 non-shared network attachment나 별도의
guest address reporting 경로가 없으며, container runtime은 항상 guest VM 내부에 남습니다.

SSH가 준비된 뒤 provisioning stage 실행 순서는 다음과 같습니다.

```text
system -> user -> guest configuration transaction -> after-boot -> ready
```

Guest transaction은 managed configuration을 바꾸기 전 runtime 관련 file과 service
state를 snapshot합니다. Docker/containerd/K3s helper step이 실패하면 그 snapshot을
복구합니다. Host는 transaction이 commit되고 Docker 및 containerd readiness check가
성공한 뒤에만 configuration fingerprint를 기록합니다.

`stop`과 `delete`는 profile이 소유한 SSH forward, Docker observer, TCP listener,
UDP relay, VM process state를 닫습니다. Soft `delete`는 disk를 보존합니다.
`delete --data`는 profile directory를 제거하기 전 interactive standard input에서
정확히 `y`를 요구합니다.

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
external kubeconfig catalog, managed kind cluster, public containerd socket, Desktop app,
XPC service, Homebrew Cask, DMG, notarization, Docker shim이 없습니다.
