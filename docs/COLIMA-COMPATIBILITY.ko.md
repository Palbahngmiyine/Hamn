# Colima 호환성

이 문서는 Hamn 0.0.1 Colima 호환성 reference의 한국어 번역입니다. 기준 영문
문서는 [COLIMA-COMPATIBILITY.md](COLIMA-COMPATIBILITY.md)입니다.

Hamn은 Apple Silicon macOS에서 local development를 위한 Docker-only 대체 제품으로
설계되었습니다. `colima` alias가 아니며 Colima VM, profile, configuration, Docker
context, socket, installation을 재사용하지 않습니다. Colima는 더 넓은 host/runtime
matrix를 지원합니다. [Colima project](https://github.com/abiosoft/colima)를
참조하세요.

## 명령 대응

| Colima 의도 | Hamn 명령 | 비고 |
| --- | --- | --- |
| 기본 instance 시작 | `hamn start` | 기본 Docker-only profile 시작 |
| 이름 있는 instance 시작 | `hamn start --profile work` | positional name 및 HAMN_PROFILE도 지원 |
| instance 중지 | `hamn stop --profile work` | Hamn이 활성화한 Docker context만 이전 값으로 복원 |
| VM 삭제, data 보존 | `hamn delete --profile work` | Soft delete는 profile disk를 보존 |
| 모든 profile data 삭제 | `hamn delete --profile work --data` | 대상/disk allocation 출력 후 정확히 y 필요 |
| instance 상태 | `hamn status --profile work --json` | Docker API, CRI, K3s readiness 분리 |
| instance 목록 | `hamn list` | Hamn profile만 표시 |
| CPU / memory / disk 설정 | `hamn configure --cpu 6 --memory 8 --disk 80` | Profile YAML에 저장 |
| profile 설정 편집 | `hamn start --edit --profile work` | $EDITOR와 strict YAML validation 사용 |
| 기본값 출력 | `hamn template` | 복사 가능한 YAML template |
| guest shell | `hamn ssh --profile work` | SSH가 host-to-guest control transport |
| Docker client 환경 | `eval "$(hamn env --profile work)"` | SDK와 Testcontainers 용도 |
| local Kubernetes 활성화 | `hamn kubernetes start --profile work` | 명시적 profile-local K3s lifecycle |
| local Kubernetes 사용 | `hamn kubectl --profile work -- get nodes` | 선택 프로필 kubeconfig만 사용 |
| amd64 translation 설정 | `hamn configure --rosetta true` | 기본은 binfmt, Rosetta는 opt-in |
| Hamn 제거 | `hamn uninstall` | managed install/runtime data 출력 후 정확히 y 필요 |

Hamn 시작 후에는 표준 Docker tool을 사용합니다.

~~~sh
hamn start --profile work
docker context show
docker compose up -d
docker buildx build --load -t example .
docker run --rm alpine uname -m
~~~

기본 Hamn Docker context는 `hamn`이고 이름 있는 profile은 `hamn-<profile>`입니다.
Docker endpoint는 `~/.hamn/<profile>/docker.sock`이며 host `/var/run/docker.sock`이
아닙니다.

## 의도적인 차이

Hamn은 공개적으로 Docker runtime만 지원합니다. Colima runtime switching,
`colima nerdctl` UX, Incus integration, Linux host, Intel Mac, GPU/AI workload,
multi-node Kubernetes는 0.0.1에 없습니다.

Hamn은 profile socket으로 Docker Engine API를 노출합니다. Guest containerd/CRI
socket은 macOS에 노출하지 않습니다. Docker는 guest `moby` namespace를 사용하고,
선택형 K3s는 같은 system containerd의 CRI를 통해 `k8s.io`를 사용합니다.
[Architecture](ARCHITECTURE.ko.md)를 참조하세요.

지원하는 네트워크 모드는 shared NAT뿐이며 profile별 bridged 또는 interface 선택은
제공하지 않습니다. Docker-published TCP port에는 SSH ControlMaster를, UDP port에는
bounded relay를 사용합니다.

## Colima 공존

Hamn의 공존 검증과 release 절차는 Colima를 변경하지 않아야 합니다. 특히 다음을 해서는
안 됩니다.

- Colima VM/profile을 start, stop, delete, edit
- Colima Docker context/socket 변경
- Colima install, update, uninstall, alias 생성
- Colima VM disk, SSH configuration, runtime state 재사용

## Migration 개요

1. 현재 Docker context를 기록하고 Colima의 실행/중지 상태를 그대로 둡니다.
2. docker executable을 교체하지 않고 Hamn을 설치합니다.
3. 이름 있는 Hamn profile을 시작하고 docker context show가 hamn-<profile>인지
   확인합니다.
4. 프로젝트의 Docker CLI, Compose, buildx, SDK, Testcontainers workflow를 Hamn
   profile에서 검증합니다.
5. Hamn을 중지하고 원래 Docker context가 복원되었는지 확인합니다.
6. Colima 전후 state를 비교합니다. 변한 state가 있으면 계속 진행하지 말고 원인을
   확인합니다.

Hamn migration 과정에서 기존 Colima setup을 삭제하거나 변경하지 마세요.
