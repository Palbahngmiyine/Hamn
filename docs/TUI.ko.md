# 작업 영역과 네이티브 명령

터미널에서 `hamn`을 실행하고 최초 실행 시 **컨테이너** 또는 **Kubernetes**를
선택합니다. 다음 실행부터 저장한 영역으로 진입합니다. Tab은 영역 전환, `,`는
기본 영역 변경입니다. 각 영역의 연결 대상과 탐색 상태는 별도로 유지합니다.

컨테이너 탐색에는 Docker CLI, Kubernetes 탐색에는 kubectl을 설치해야 합니다.
Compose·buildx·kubectl 플러그인·exec 인증 도구도 외부 설치 의존성입니다.
Hamn은 하나의 실행 파일을 유지합니다. 헤드리스 SDK 작업은 기존 계약을 유지하며
이 CLI 설치를 요구하지 않습니다.

## 탐색

| 키 | 동작 |
| --- | --- |
| `:` | Docker 또는 kubectl 명령 입력 |
| `/`, 방향키·`j/k` | 표시된 행 검색 및 리소스 선택 |
| Enter, `l`, `g` | 지원하는 리소스의 상세·로그·통계 |
| `s`, `t`, `r`, `d` | 지원하는 리소스의 시작·정지·재시작·삭제 |
| `m` 또는 `?` | 작업 및 명령 도움말 |
| Tab, `,` | 영역 전환, 기본 영역 변경 |
| `e` | Hamn 프로필·외부 Docker context 또는 Kubernetes context 선택 |
| `n` | Kubernetes namespace 선택 |
| `a` | 현재 조회 필터를 유지하며 전체 컨테이너 표시 전환 |
| `v` | 선택한 Hamn 환경의 VM 패널 |
| VM 패널의 `c` | CPU·메모리(GiB)·디스크(GiB) 설정 명령 편집 |
| `!` | 진행 중인 수명 주기 작업 로그 |
| Esc, `q` | 복귀, 종료 |

컨테이너 영역은 실행 중인 컨테이너부터 표시합니다. 정지된 Hamn 환경은 시작
동작을 제공하며, 목록 조회만으로 VM을 부팅하지 않습니다. VM 패널은 VM 상태와
Docker 준비 상태를 구분합니다. 외부 Docker context에는 Hamn VM 제어가 없습니다.
`a` 키는 `-as` 같은 결합 옵션과 반복 boolean 옵션에서도 Docker의 `--all` 값을
전환합니다. 필터·크기·`--last`·`--latest` 옵션은 유지하며, 마지막 두 옵션은
Docker의 원래 동작대로 모든 컨테이너 상태를 조회 범위에 포함합니다.
Kubernetes는 현재 유효한 context의 Pod 목록, 없으면 context 선택기로 시작합니다.
Kubernetes 진입은 VM 시작이나 Hamn 프로필 마이그레이션을 실행하지 않습니다.
context·namespace 선택은 kubeconfig나 Docker 설정을 변경하지 않습니다.
환경·context·namespace 선택기에서 Esc를 누르면 선택을 취소하고 이전 조회,
명시적 연결 옵션, 행 필터, 선택과 스크롤 위치를 복원합니다. Enter로 대상을
선택하거나 새 리소스 조회 명령을 입력하면 새 화면으로 전환합니다.
선택기를 이동하거나 취소해도 수명 주기 작업의 진행 상태와 로그는 되돌리지 않습니다.
환경 선택기는 Hamn 프로필과 Docker context를 구분하고 외부 연결 주소를 표시합니다.
Enter로 대상을 먼저 선택한 뒤 리소스 작업을 사용합니다. 선택기 안에서는 VM 작업과
설정 단축키를 제공하지 않습니다.

## 명령

해당 영역에서는 `docker`·`kubectl` 접두사를 생략할 수 있습니다. 전체 접두사를
입력하면 해당 영역으로도 전환합니다.

```text
ps
ps -a --filter label=app=api
docker images
volume ls
network ls
pods
get pods -A
kubectl get deployments -n dev --sort-by=.metadata.name
```

지원하는 일반 목록 조회는 설치된 CLI의 JSON 출력을 선택 가능한 표로 표시합니다.
`ps --size`(`-as` 포함)는 Size 열을 추가하며, 반복한 불리언 옵션은 CLI처럼 마지막
값을 적용합니다. `get pods -Aoyaml`·`get pods -Aw`처럼 결합한 kubectl 출력·watch
옵션도 내부 터미널에서 원래 인자를 유지하여 실행합니다.
`docker -DHunix:///path/docker.sock ps`·`get pods -Ashttps://api.example` 같은 결합
연결 옵션도 명시한 대상을 상단 표시와 선택한 리소스 작업에 유지합니다. 입력한
명령의 원래 인자는 그대로 전달합니다.
`--all-namespaces`·`-A`는 마지막 불리언 값으로 범위를 결정합니다. 최종 값이
`false`이면 명시한 대상 옵션이 우선하는 경우를 제외하고 UI namespace 기본값을 유지합니다.
옵션 값은 다른 옵션으로 해석하지 않습니다. `--as-group -nteam`은
`--as-group=-nteam`처럼 선택한 namespace를 유지합니다. 값이 `--context`·
`--kubeconfig`·출력 옵션과 비슷해도 UI 기본 대상이나 목록 형식을 바꾸지 않습니다.
`create configmap example --from-literal --namespace=value`처럼 내장 명령에 전달하는
데이터에도 적용됩니다. 이 토큰은 선택한 namespace에 들어갈 데이터로 유지합니다.
`logs -f`의 follow와 `get -f`의 파일 이름처럼 명령별 옵션 의미도 구분합니다.
필터·조회 범위·정렬은 CLI가 처리합니다. `--format`, `-q`, `-o yaml` 등 명시한 출력
옵션은 유지하고 터미널에 표시합니다. 그 밖의 명령도 원래 CLI로 전달합니다.
Compose·buildx·`exec -it`·`attach`·`logs -f`·`stats`·`apply`·`edit`·`port-forward`를
포함합니다. `containers` 같은 이전 명령은 호환 별칭으로 남습니다.
Docker의 `--digests`, `--no-trunc`, `--tree` 조회도 터미널로 표시하여 요청한
digest 필드·전체 식별자·트리 모양을 CLI 출력 그대로 유지합니다.

입력한 명령은 한 번 실행하며 Hamn의 추가 확인이나 명령 제한 시간을 삽입하지
않습니다. 작업 메뉴에서 선택한 변경에는 확인을 유지합니다. Kubernetes 메뉴 변경은
선택한 UID와 리소스 버전을 유지합니다. 삭제는 kubectl로 서버 전제조건을 전달하고,
재시작은 원자적인 조건부 patch를 사용합니다. 리소스가 바뀌면 다시 조회해 선택하세요.
연결 대상을 바꾸면 새 목록이 도착할 때까지 이전 행을 무효화합니다.
`kubectl events`와 `events`는 원래 명령 의미를 유지하며, 표 조회는 `get events`를 사용합니다. 따옴표·이스케이프는
지원하며 셸 파이프·리다이렉션·변수 확장·셸 별칭은 해석하지 않습니다. 필요하면
`exec` 안에서 셸을 명시적으로 실행하세요. 구조화 조회 출력은 16 MiB로 제한하며
초과하면 오류를 표시합니다.

UI 선택은 연결 기본값입니다. 명시한 Docker `--context`·`--host`, kubectl
`--context`·`--kubeconfig`·`--namespace`·`-n`이 우선하며 `-A`의 전체 namespace 범위를
유지합니다. Docker 전역 옵션은 CLI 규칙대로 명령 앞에 둡니다. 상단에는 실제 호출
대상을 표시합니다. `docker context use`·`kubectl config`는 원래 설정 변경 의미대로
실행하고 터미널에서 복귀하면 선택 정보를 다시 읽습니다.
kubectl의 `--cluster`로 context의 기본 cluster를 덮어쓰면 목록·터미널·선택 작업
확인 화면에 context와 함께 해당 cluster 옵션도 표시합니다.
선택한 리소스의 작업에도 조회에 지정한 TLS 서버 이름·인증서·인증·사용자 가장·
프록시 옵션을 유지합니다.

설치된 kubectl 플러그인은 자체 인자 문법을 가집니다. kubectl은 플러그인 이름 앞의
옵션을 거부하며 플러그인도 context 옵션을 지원하지 않을 수 있어, Hamn은 UI의
context·namespace 옵션을 삽입하지 않고 원래 인자를 전달합니다. 상단에
**Plugin-defined target / inherited CLI configuration**을 표시합니다. 플러그인의
연결 옵션은 해당 플러그인 규칙에 따라 지정하세요. 플러그인 지원이 모든 플러그인의
대상을 UI 선택으로 강제한다는 뜻은 아닙니다.
설치된 `kubectl-ns`·`kubectl-pods`·`kubectl-ctx`·`kubectl-contexts`는 Hamn의 같은
이름 편의 별칭보다 우선합니다. `kubectl` 접두사나 추가 인자가 없어도 동일합니다.
플러그인이 없으면 인자 없는 `ctx`·`contexts`는 기존 context 목록을 엽니다.
`kubectl create <extension>` 플러그인도 원래 인자를 유지합니다. 내장 create 명령과
그 별칭은 같은 이름의 플러그인 파일보다 우선합니다.

연결 규칙은 공식 [Docker CLI 문서](https://docs.docker.com/reference/cli/docker/),
[kubectl 문서](https://kubernetes.io/docs/reference/kubectl/),
[kubectl 플러그인 계약](https://kubernetes.io/docs/tasks/extend-kubectl/kubectl-plugins/)을
기준으로 합니다.

## 내부 터미널과 수명 주기 작업

내부 PTY는 터미널 입출력을 제공하고 창 크기를 따라 변경됩니다. Ctrl-C는 CLI로,
Docker 기본 detach 키 Ctrl-P Ctrl-Q도 그대로 전달됩니다. 명령이 끝나면 종료
코드를 표시합니다. Enter 또는 Esc로 원래 탐색 화면에 돌아가 리소스를 갱신합니다.
실행 중에는 Shift+PageUp/PageDown으로 이전 출력을 보고, 종료 후에는
PageUp/PageDown도 사용할 수 있습니다. 일반 입력을 보내면 최신 출력으로 돌아갑니다.
입력은 최대 4 MiB까지 순서대로 대기합니다. 한도를 넘는 붙여넣기·키 입력은 전체를
거부하고 메시지를 표시합니다. Ctrl-C는 원래 CLI의 바이트 입력 의미를 유지합니다.
Ctrl+Alt+C는 대기 입력과 터미널 입력 버퍼를 비우고 CLI 프로세스 그룹에 SIGINT를
보냅니다. 버린 대기 바이트 수와 CLI 종료·쓰기 실패로 전달하지 못한 입력을 표시합니다.
`!` 작업 로그에서는 방향키 또는 `j/k`로 로그를 스크롤합니다.

VM 시작·정지·복구는 목록 조회와 별도로 실행합니다. 화면 전환·새로고침·일반 Esc로
취소하지 않습니다. 실행 중 종료를 요청하면 취소 후 종료를 확인하고 자식 프로세스
종료와 정리를 기다립니다. 시작 취소는 원격 정리가 확인된 뒤 그 작업이 생성한 VM만
정지합니다. 완료를 확인하지 못하면 VM을 보존하고 복구 필요를 표시합니다. 실행 중
로그는 최근 1 MiB를 유지하며 실행 중에도 볼 수 있습니다. 화면 처리가 밀리면 작업
로그 전달을 대기하여 큐의 로그를 조용히 버리지 않습니다. worker 종료 후에는 큐에
남은 바이트를 수집하며, stderr를 보유한 백그라운드 프로세스의 종료를 기다리지
않습니다. 화면 처리 지연 때문에 이 수집이 만료되지 않습니다. 결과 없이 worker가 종료되면
오류에 마지막 8 KiB의 진단을 포함합니다. 외부 변경을 CLI 종료만으로 되돌렸다고 표시하지 않습니다.
SSH 변경 작업이 오류로 끝났을 때도 해당 요청 토큰의 실행을 차단한 뒤 원격 작업
종료를 기다립니다. 정리 여부를 확인하지 못하면 해당 작업의 후속 게스트 변경을
막고 복구를 위해 VM을 보존합니다.

VM 중지가 성공해도 앞선 retirement 경고가 함께 반환될 수 있습니다. 작업 상태와
로그는 이 경고를 유지하며, `outcomeUnknown`이면 원래 프로필과 `vm migrate`의
진단을 `!`에도 남깁니다. 확정 실패는 경고로 표시하고 결과 불명으로 바꾸지 않습니다.
다른 작업 영역을 보고 있어도 결과는 컨테이너 영역에 남으며, 종료 시 터미널을
복원한 뒤에도 출력합니다.
긴 진단은 상단에 한 줄 요약과 `! log` 안내로 표시합니다. 전체 내용은 스크롤 가능한
작업 상세에 유지하며, 오류 메시지 길이 때문에 리소스 목록이 가려지지 않게 합니다.

VM 실행 중이라는 사실만으로 Docker 사용 가능 상태를 보장하지 않습니다. 준비 상태는
사용 가능·준비 중·연결 불가·복구 필요를 구분하며 시작 성공에는 호스트 소켓과 실제
Docker `/_ping` 응답이 필요합니다. 중단된 작업의 식별자와 결과는 다음 실행에서도
확인할 수 있게 보존합니다. VM 변경 전 입력 검증 거부는 확정 실패로 기록하며,
그 자체로 새로운 복구 필요 경고를 만들지 않습니다. [API](API.ko.md)를 참고하세요.

K3s 정리 완료와 Docker 배포 미완료는 별도로 판단합니다. 소유권·완전성·helper 계약과
정리 단계가 일치하는 백업은 rollback 후 재시도합니다. 구형 백업은 신뢰하는 helper
식별자와 전체 메타데이터를 검사합니다. 모호하거나 불완전하거나 변조된 백업은
오류와 함께 보존합니다. 복구는 Docker 컨테이너·이미지·볼륨을 삭제하지 않습니다.

## 환경설정

`~/.hamn/tui.json`에는 `{"version":1,"defaultWorkspace":"containers"}` 또는
`"kubernetes"`를 저장합니다. 사용자 전용 임시 파일·파일 동기화·원자적 rename·
디렉터리 동기화와 `0600` 권한을 적용합니다. 잘못된 버전·JSON·권한·심볼릭 링크
읽기는 오류를 표시하고 선택 화면으로 돌아갑니다. 다시 선택하면 유효한 설정 파일을
저장합니다. 연결 대상 선택은 세션 동안 유지하며 이 파일은 기본 영역만 저장합니다.
