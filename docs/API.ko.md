# 제어 API

공개 인터페이스는 `hamn --headless <operation> [인자]`입니다. TUI VM 제어도 같은 서비스를
호출합니다. 네이티브 Docker·kubectl 명령은 PTY를 통해 외부 CLI를 실행하며
헤드리스 작업을 추가하지 않습니다.
`hamn --headless capabilities`가 지원 작업과 변경 여부를 반환합니다.

## 응답 계약

```json
{"schemaVersion":1,"requestId":"example","ok":true,"target":{"profile":"work","context":null,"namespace":null,"name":null},"data":[],"error":null}
```

실패는 `ok:false`, `data:null`, `error:{"code":"...","message":"..."}`를 반환합니다.
프로그램은 오류 코드를 사용하고 메시지는 진단용으로 취급하세요. 실패 시 프로세스 종료
코드는 0이 아닙니다. 인자 파싱·터미널 오류도 JSON이며 `--help`·`--version`은 텍스트입니다.

`--watch`는 2초마다 조회를 반복하고, `--follow`는 컨테이너·Pod 로그를 스트리밍합니다.
`--follow`가 없는 로그 조회도 NDJSON을 사용합니다. 각 NDJSON 레코드는 공통 필드와 `type`·`sequence`를 포함합니다. 로그를 모두 출력한 뒤
최종 결과를 출력합니다. 제한된 큐로 생산 속도를 조절하며 로그 한 줄과 일회성 Docker
로그 응답은 최대 1 MiB입니다. `--tail`은 0–10000, 기본값은 200입니다.

`--timeout`은 초 단위 제한 시간이며 기본값 600, 범위 1–3600입니다. Ctrl-C·SIGTERM은
취소를 요청합니다. 전달 후 취소한 외부 변경은 `outcomeUnknown`을 반환할 수 있습니다.
매니지드 VM 취소는 정리를 기다리고 복구가 확인된 경우에만 `cancelled`로 표시합니다.
서버가 이미 변경을 반영했을 수 있으므로 재시도 전에 대상 상태를 조회하세요.
Hamn은 서버가 수락한 변경을 되돌렸다고 보고하지 않습니다.

## VM 준비 상태와 작업 기록

기존 VM 상태 필드를 유지하며 `dockerStatus`(`ready`, `preparing`, `unavailable`,
`recoveryRequired`)와 `lastOperation`(기록이 없으면 null)을 추가합니다.
`state:running`은 VM 프로세스 상태이며 `ready`에는 Docker `/_ping` 확인이 필요합니다.
`lastOperation`에는 `schemaVersion`, `operationId`, `operation`, `status`, `phase`,
`startedVm`, `exitCode`, `error`가 포함됩니다. 실행 중에는 종료·오류 필드가 없을 수 있습니다.
소유권은 PID·프로세스 시작 시각·실행 파일 UUID를 함께 검사하며 PID만 신뢰하지 않습니다.
완료·실패·취소·결과 불명 상태는 사용자 전용 원자적 파일
`~/.hamn/<profile>/operation.json`에 남습니다. 소유 프로세스가 사라진 실행 기록은
`outcomeUnknown`·`recoveryRequired`로 표시합니다. 화면 이동은 작업을 취소하지 않으며
종료 확인 후 취소와 정리를 기다립니다. 결과가 불명이면 재시도 전에 상태를 확인하세요.

## 지원 작업

| 영역 | 작업 |
| --- | --- |
| VM | `vm list`, `status`, `create`, `configure`, `start`, `stop`, `delete`, `migrate`, `diagnostics`, `env` |
| Docker 컨테이너 | `docker containers list`, `inspect`, `logs`, `stats`, `start`, `stop`, `restart`, `delete` |
| Docker 목록 | `docker images list`, `docker volumes list`, `docker networks list` |
| Kubernetes 선택 | `k8s contexts list`, `k8s namespaces list` |
| Kubernetes 목록 | `k8s <resource> list`; pods, deployments, statefulsets, daemonsets, services, nodes, events, jobs, cronjobs, ingresses, pvcs |
| Kubernetes 상세 | 위 목록의 모든 리소스와 namespaces에 `k8s <resource> inspect <name>` 지원; 객체 JSON과 YAML |
| Kubernetes 로그 | `k8s pods logs` |
| Kubernetes 변경 | `k8s deployments scale/restart`, `statefulsets scale/restart`, `daemonsets restart`, `pods delete` |
| 유지관리 | `system update`, `system uninstall` |

VM·Docker 작업은 `vm list`를 제외하고 `--profile`이 필요합니다. 생성·설정은
`--cpu`, `--memory`(GiB), `--disk`(GiB)를 받습니다. `vm diagnostics`의 `--path`는
아카이브 경로이며 `system update`는 `--manifest`를 지원합니다.
`vm env`는 셸 코드 대신 Docker 접속 정보를 반환합니다.

Kubernetes는 context 목록을 제외하고 `--context`가 필요합니다. 네임스페이스 변경
작업에는 `--namespace`도 필요하며 목록은 `--all-namespaces`를 지원합니다.
대상 이름은 작업 뒤에 쓰거나 `--name`으로 지정합니다. `--uid`는 교체된 Kubernetes
객체에 대한 조작을 방지합니다. 스케일에는 `--replicas`가 필요하고 0도 허용합니다.
Pod 로그에는 `--container`·`--previous`를 사용할 수 있습니다.

모든 헤드리스 변경에는 `--yes`가 필요하며 TUI VM 제어는 확인 후 이를 전달합니다.
입력한 네이티브 CLI 명령은 자체 확인·출력 의미를 유지합니다. 변경에는
`--watch`, `--follow`, `--all-namespaces`를 사용할 수 없습니다.

## 연결과 소유권

Docker 요청은 API 버전 협상 후 `~/.hamn/<profile>/docker.sock`으로 직접 전달합니다.
변경 전 컨테이너 이름을 고정 ID로 해석하고, 컨테이너 삭제 시 볼륨은 보존합니다.
Docker 공개 포트의 포워딩은 기존 C 관찰기가 소유하며 외부 도구도 같은 소켓을 사용합니다.

Kubernetes 설정은 `--kubeconfig`, `KUBECONFIG`, `~/.kube/config` 순서로 읽습니다.
context·namespace 선택은 파일에 쓰지 않습니다. 인증서·토큰·exec 인증 플러그인은
kube-rs를 사용합니다. Hamn은 클라이언트를 만들기 전에 exec 인증을 비동기로
수행하며 대화형 인증은 거부합니다. Exec 인증은 터미널 입력 없이 출력 크기·작업 시간 제한을 적용합니다. 작업과 watch
조회마다 인증을 다시 수행합니다. 구형 `auth-provider` 설정은 exec 인증으로
교체해야 합니다.

C VM 작업은 같은 실행 파일의 새 프로세스에서 수행합니다. `host/core/control.h`의
ABI는 입력 문자열을 빌리고, 반환하는 UTF-8 JSON의 소유권은 호출자에게 넘깁니다.
반환 메모리는 `hamn_control_free`로 해제합니다. JSON은 C 상태에서 직접 생성하며
기존 CLI 출력을 파싱하지 않습니다. C stdout은 worker 프로토콜과 분리하고,
프로세스 식별 검증·수명주기 잠금은 C가 유지합니다.

`__core-worker`, `vmrun`, 포워딩 프로세스 모드, 게스트 `hamnd` 엔드포인트는 내부
구현입니다. 공개 containerd 소켓, 내장 Compose·exec·apply·port-forward,
MCP 서버는 제공하지 않습니다.

구현에 적용한 상위 계약은 [Cargo 정적 링크](https://doc.rust-lang.org/cargo/reference/build-script-examples.html#building-a-native-library),
[Ratatui 백엔드](https://ratatui.rs/concepts/backends/),
[kubeconfig 규칙](https://kubernetes.io/docs/concepts/configuration/organize-cluster-access-kubeconfig/)을 참고하세요.
