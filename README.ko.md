# Hamn

Hamn은 하나의 macOS 실행 파일로 Linux VM, VM의 Docker Engine, 외부 Kubernetes
클러스터를 관리합니다. `hamn`은 Ratatui 터미널 화면을 열고,
`hamn --headless`는 자동화를 위한 JSON·NDJSON을 출력합니다.

## 요구 사항

- macOS 13 이상의 Apple Silicon Mac.
- VM·Docker 기능에는 서명된 Hamn 게스트 이미지가 필요합니다.
- Kubernetes에는 kubeconfig가 필요하며 Hamn VM과 독립적으로 사용할 수 있습니다.

TUI 컨테이너 탐색에는 Docker CLI, Kubernetes 탐색에는 kubectl이 필요합니다.
프로필 Docker·Kubernetes 헤드리스 SDK 작업은 이 CLI들을 요구하지 않지만,
외부 Docker `--context` 요청에는 Docker CLI가 필요합니다. Compose·buildx·플러그인은
외부 설치 의존성이며 Docker Desktop은 필요하지 않습니다.

## 설치

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

설치기는 Hamn과 Linux 게스트 이미지를 내려받아 둘 다 검증한 뒤 `hamn` 명령을
`~/.local/bin`에 설치합니다. 출력 예시는 다음과 같습니다.

```text
Installing Hamn 0.1.2 for Apple Silicon macOS...
Downloading Hamn 0.1.2 (4.4 MiB)...
Downloading guest image  100%  1.1 GiB / 1.1 GiB  11.8 MiB/s
Installing...
Installed Hamn 0.1.2.
Run hamn to get started. Update later with hamn upgrade.
```

`~/.local/bin`이 PATH에 없으면 사용 중인 shell에 추가할 한 줄을 알려 줍니다.
macOS 기본 도구만 사용하므로 Python, Homebrew, Rust, Xcode Command Line Tools를
따로 설치할 필요가 없습니다. Hamn 0.1.2 이하는 스스로 이 릴리스로 업데이트할 수
없으므로 위 설치기를 한 번 실행합니다. 설치기가 기존 설치를 제자리에서 이전합니다.

## 터미널 화면

처음에 컨테이너 또는 Kubernetes를 선택합니다. Tab으로 영역을 전환하고 `,`로
기본 영역을 변경합니다. `:ps`, `:docker ps -a`, `:images`, `:get pods -A`,
`:kubectl get deployments -n dev`를 입력할 수 있습니다. 일반 목록은 선택 가능한
표로 표시하고 나머지는 내부 터미널에서 원래 CLI 의미대로 실행합니다.
`e`는 환경·context 선택, `v`는 Hamn 프로필의 VM 제어입니다.

## 헤드리스 인터페이스

```sh
hamn --headless capabilities
hamn --headless vm create --profile work --cpu 4 --memory 4 --yes
hamn --headless vm start --profile work --yes
hamn --headless docker containers list --profile work
hamn --headless docker containers list --context remote
hamn --headless docker containers logs api --profile work --follow
hamn --headless k8s contexts list
hamn --headless k8s pods list --context dev --namespace default
hamn --headless k8s deployments scale api --replicas 3 --context dev --namespace default --yes
hamn --headless vm stop --profile work --yes
```

변경에는 명시적 대상과 `--yes`가 필요합니다. 일회성 작업은 JSON 응답 하나,
`--follow` 로그와 `--watch` 조회는 NDJSON을 출력합니다. stdout에는 기계용 응답만
출력합니다.
`vm list`는 `--yes` 없이 실행되지만 삭제도 합니다. `vm up`이 만든 프로필 가운데 워크스페이스
경로가 더 이상 없고 VM이 멈춰 있는 프로필을 디스크·Docker 데이터와 함께 지웁니다(ADE와 함께
쓰기 참고).

## 호스트 여유

`vm start`는 VM을 띄우기 전에 호스트의 메모리 압박 단계, 메모리, 1분 부하, CPU 수,
전원 상태를 읽고, 실행 중인 Hamn VM에 설정된 메모리를 더합니다. 호스트가 새 VM을
감당할 수 없으면 `admissionRefused`로 실패하고 VM, 설정, 디스크를 그대로 둡니다.
자리를 만들려고 다른 VM을 멈추지 않습니다. 헤드리스 오류의 `details`에는 거부 사유,
측정값, 한도, 실행 중인 프로필이 들어 있고, TUI는 같은 메시지를 보여 줍니다.
메모리 압박, 메모리, 실행 중인 VM의 메모리나 켜진 한도가 쓰는 측정값을 읽지 못해도
시작을 거부합니다. 동시에 들어온 시작은 하나씩 판정하므로 두 요청이 같은 여유
메모리를 함께 쓰지 않습니다.

한도는 `hamn`을 실행하는 환경 변수로 정합니다.

| 변수 | 시작을 거부하는 경우 | 기본값 |
|---|---|---|
| `HAMN_ADMISSION_MEMORY_RESERVE_MIB` | 실행 중인 VM과 새 VM의 메모리가 호스트 메모리에서 이 MiB를 뺀 값보다 큼 | `0` |
| `HAMN_ADMISSION_MEMORY_PRESSURE` | macOS 메모리 압박이 이 단계 이상: `warn` 또는 `critical` | `critical` |
| `HAMN_ADMISSION_MAX_LOAD_PER_CPU` | CPU 하나당 1분 부하가 이 값(예: `1.5`)보다 큼 | 한도 없음 |
| `HAMN_ADMISSION_MIN_BATTERY_PERCENT` | 배터리 전원이고 잔량이 이 퍼센트보다 낮음 | `0`, 한도 없음 |

기본 한도는 기준 측정으로 실제 값을 정하기 전까지 일부러 느슨하게 두었습니다.

## 선언적 설정

프로필의 모든 설정을 파일 하나에 적어 두고, `vm apply`로 프로필을 그 파일과 같게
만듭니다. 파일 `work.yaml`의 예입니다.

```yaml
apiVersion: hamn/v1
kind: Profile
metadata:
  name: work
spec:
  cpus: 6
  memoryMiB: 8192
  diskGiB: 80
  rosetta: true
  mounts:
    - location: "/Users/<your-user>/project"
      mountPoint: "/workspace/project"
      writable: true
```

```sh
hamn --headless vm apply --profile work --file work.yaml --dry-run
hamn --headless vm apply --profile work --file work.yaml --yes
hamn --headless vm start --profile work --yes
```

첫 번째 명령은 달라질 내용을 알려 주며 아무것도 쓰지 않습니다. 두 번째 명령은
프로필이 없으면 만들고, 파일과 설정이 다르면 설정을 교체합니다. 다른 점이 없으면
아무것도 쓰지 않으므로 VM이 실행 중일 때를 포함해 언제든 다시 실행할 수 있습니다.

- `metadata.name`은 `--profile`과 같아야 합니다.
- `spec`에는 프로필의 `~/.hamn/<profile>/config.yaml`과 같은 키를 적습니다. 정의 파일이
  설정 전체이므로 `spec`에서 생략한 키는 기본값으로 돌아갑니다. 디스크는 줄일 수
  없으므로 `diskGiB`는 파일에 계속 적어 두세요. 현재 크기보다 작은 값은 키를 생략했을
  때의 기본값 60을 포함해 거절합니다.
- `vm apply`는 설정만 바꿉니다. VM을 시작하거나 정지하지 않으며, 설정을 바꾸려면
  VM이 정지되어 있어야 합니다. `vm stop`, `vm apply`, `vm start` 순서로 실행하세요.
- `vm apply`는 Mac의 조건을 검사하지 않으며 다음 `vm start`가 검사합니다. 마운트의
  `location`은 실제로 있는 본인 소유 디렉터리로 바꾸고, 쓰기 가능한 마운트라면 홈
  디렉터리 안의 디렉터리를 쓰세요. `rosetta: true`는 Rosetta를 쓸 수 있는 Mac에서만
  두세요.

## 워크스페이스마다 VM 하나

`vm up`은 git 워크트리 같은 디렉터리 하나에 전용 VM을 주고, `vm down`은 그 VM을
멈추거나 지웁니다. 프로필 이름 대신 디렉터리의 절대 경로를 넘깁니다.

```sh
hamn --headless vm up --workspace "$PWD" --yes
hamn --headless vm down --workspace "$PWD" --yes
hamn --headless vm down --workspace "$PWD" --purge --yes
```

- 프로필 이름은 `ws-`와 디렉터리 실제 경로에서 얻은 16진수 12자리입니다. 대소문자를
  구분하지 않는 디스크의 `.../Project`와 `.../project`처럼 한 디렉터리를 가리키는
  경로는 모두 같은 VM을 가리킵니다. 같은 디렉터리로 `vm up`을 다시 실행하면 그
  프로필을 다시 시작하고 같은 프로필과 Docker 소켓을 돌려줍니다.
- 새 프로필은 홈 디렉터리를 공유하지 않습니다. 워크스페이스는 쓰기 가능하게, 워크트리의
  공용 git 디렉터리는 읽기 전용으로 VM 안의 같은 경로에 공유합니다. `--no-git-mount`를
  주면 워크스페이스만 공유합니다. `--cpu`, `--memory`, `--disk`는 새 프로필의 자원을 정하고,
  이미 있는 프로필은 `vm start`처럼 바꿉니다.
- git 디렉터리는 그 워크스페이스의 것일 때만 공유합니다. 워크스페이스 안의 `.git`
  디렉터리이거나, git이 이 워크트리를 위해 만든 연결된 워크트리의 git 디렉터리여야 합니다.
  서브모듈, 다른 저장소를 가리키는 `.git` 파일, 심볼릭 링크인 `.git`이면 경고를 남기고
  워크스페이스만 공유합니다. 읽기 전용 git 공유는 게스트 안에서 git을 쓰기 위한 편의일 뿐
  게스트에 대한 보안 경계가 아닙니다. 일반 저장소에서는 `.git`이 쓰기 가능한 워크스페이스
  공유 안에 있기 때문입니다.
- 응답은 프로필의 `vm status`에 `profile`, `workspace`, 그리고 `DOCKER_HOST`에 넣을
  `unix://` 값인 `dockerHost`를 더한 것입니다.
- 호스트가 VM을 감당할 수 없거나(`admissionRefused`) 첫 시작이 실패하면 `vm up`은 자신이
  만든 프로필을 지웁니다. 이미 있던 프로필은 그대로 둡니다.
- `vm down`은 VM을 멈추고 디스크를 남기며, `--purge`는 `vm delete --purge`처럼 프로필을
  지웁니다. 둘 다 워크스페이스와 그 저장소는 건드리지 않습니다.
- 둘 다 `vm up`이 이 디렉터리를 위해 만든 프로필에만 작동합니다. 같은 이름이지만 그
  기록이 없는 프로필이나, 같은 경로에 예전에 있던 디렉터리의 프로필은 `conflict`로
  거부합니다.
- 워크스페이스는 실제로 있는 본인 소유 디렉터리여야 하며, 쓰기 가능한 공유가 모두
  그렇듯 홈 디렉터리 안에 있어야 합니다.

## ADE와 함께 쓰기

T3 Code와 Orca는 워크트리를 만들 때와 그 워크트리로 할 일을 마쳤을 때 사용자가 정한
명령을 실행합니다. 명령에는 각 도구가 넣어 주는 워크트리 경로를 넘깁니다. `$PWD`는 쓰지
않습니다. 도구가 프로젝트 루트에서 명령을 실행하면 원래 체크아웃에 VM이 생깁니다.

T3 Code에서는 프로젝트의 `t3.json`에 스크립트 두 개를 더합니다.

- `"runOnWorktreeCreate": true`와 `"async": false`로, VM이 준비된 뒤 에이전트가
  시작되게 합니다:
  `hamn --headless vm up --workspace "$T3CODE_WORKTREE_PATH" --yes`
- `"runOnSettle": true`로:
  `hamn --headless vm down --workspace "$T3CODE_WORKTREE_PATH" --yes`

T3 Code는 스레드가 settle될 때마다 이 스크립트를 실행하고, 스레드는 그 뒤에도 이어질 수
있습니다. `vm down`은 VM의 메모리를 비우고 디스크는 남깁니다. T3 Code는 생성 스크립트를
다시 실행하지 않으므로, 이어지는 스레드에서는 에이전트나 사용자가 같은 `vm up` 명령으로
VM을 다시 시작합니다. 디스크까지 지우려면 `--purge`를 더합니다.

Orca에서는 저장소의 setup과 archive 스크립트를 `orca.yaml`에 둡니다. 훅이 켜져 있으면
(CLI는 `--run-hooks`) Orca는 워크트리를 지우기 전에 archive 스크립트를 실행합니다. 스크립트가
실패하면 사용자가 그 실패를 받아들일 때까지 워크트리를 지우지 않습니다.

```yaml
scripts:
  setup: hamn --headless vm up --workspace "$ORCA_WORKTREE_PATH" --yes
  archive: hamn --headless vm down --workspace "$ORCA_WORKTREE_PATH" --purge --yes
```

ADE에서 워크트리를 지우면 그 VM도 지워집니다. VM이 실행 중이면 Hamn은 `vm up`이 기록한
워크스페이스 경로를 10초마다 확인하고, 그 경로에 아무것도 남지 않으면 VM을 멈춘 뒤 프로필을
지웁니다. 이미 멈춰 있던 VM의 프로필은 다음 `vm list`나 `vm up`이 지웁니다. Hamn은 `vm up`이
만든 프로필만 지우며, 워크트리와 그 브랜치는 지우지 않습니다.

Hamn은 옮기거나 마운트를 해제한 워크스페이스와 지운 워크스페이스를 구별하지 못합니다. VM이 실행
중일 때 워크트리를 옮기거나(`git worktree move`, `mv`) 그 볼륨의 마운트를 해제하면 Hamn은 그
VM을 멈추고 프로필을 안의 컨테이너·이미지·볼륨과 함께 지웁니다. 멈춰 있던 VM이면 다음 `vm list`나
`vm up`이 같은 일을 합니다. Hamn이 프로필을 지우기 전에 같은 디렉터리가 그 경로로 돌아오면
프로필은 남습니다. 프로필 이름은 경로에서 나오므로, 새 경로에서 `vm up`을 실행하면 새 VM이
생깁니다.

## 외부 도구 연결

```sh
export DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock"
docker compose up -d
docker buildx build --load -t example .
```

UI 선택은 Docker의 현재 context나 kubeconfig의 `current-context`를 바꾸지 않습니다.
명시적 `docker context use`·`kubectl config`는 원래 설정 변경 의미를 유지합니다.
헤드리스 인증은 비대화형이며, 네이티브 CLI 인증은 내부 터미널에서 해당 CLI 규칙을 따릅니다.

## 데이터

`vm delete`는 VM을 멈추고 목록에서 숨기며 디스크·Docker 데이터를 보존합니다.
`vm delete --profile <이름> --purge --yes`는 VM을 멈추고 프로필을 설정·로그·디스크와 디스크 안의
컨테이너·이미지·볼륨·파일까지 영구 삭제합니다. 공유한 호스트 디렉터리는 그대로 남습니다. 없는
프로필을 purge하면 아무것도 바꾸지 않고 성공합니다.
`vm reset --profile <이름> --yes`는 VM을 멈추고 그 디스크를 안의 컨테이너·이미지·볼륨·파일과 함께
영구 삭제합니다. 다음 시작은 서명된 게스트 이미지로 새 디스크를 만들며, 프로필 설정과 공유한
호스트 디렉터리는 그대로 남습니다.
`system uninstall --yes`는 모든 Hamn 프로필과 관리 설치 파일을 영구 삭제합니다.

시작은 게스트에 무엇을 바꾸기 전에 게스트 루트 파일시스템을 먼저 확인합니다.
ext4가 오류를 기록했거나, 게스트가 자기 디스크를 읽지 못하거나, 루트가 읽기 전용이면
`guestFilesystemDamaged`로 실패하고, 자신이 부팅한 VM은 다시 멈추며, `vm status`는
그 근거를 `guestFilesystem`으로 보여 줍니다. 이런 디스크는 다시 시작해도 고쳐지지 않으며
`vm reset`으로 교체합니다.

컨테이너 생성·Compose·exec·Kubernetes apply·port-forward는 TUI에서 외부 CLI로
실행하며 헤드리스 SDK 작업 집합에는 추가하지 않습니다. MCP 서버는 제공하지 않습니다.


## 업그레이드

```sh
hamn upgrade           # 최신 릴리스 설치
hamn upgrade --check   # 새 버전이 있는지만 확인
```

```text
$ hamn upgrade
Checking for updates...
Updating Hamn 0.1.1 → 0.1.2...
Downloading Hamn 0.1.2  100%  4.4 MiB / 4.4 MiB
Downloading guest image  100%  1.1 GiB / 1.1 GiB  11.8 MiB/s
Installing...
Updated Hamn 0.1.1 → 0.1.2. Existing VMs were not restarted.

$ hamn upgrade
Checking for updates...
Hamn 0.1.2 is up to date.
```

실행 중인 VM은 재시작하지 않고 기존 VM 디스크도 바꾸지 않습니다. 새 게스트
이미지는 이후에 만드는 VM에 사용합니다. 중단된 다운로드는 자동으로 이어받거나
다음 `hamn upgrade`에서 이어받습니다. 실패하면 원인과 다음 조치를 한 줄로
표시합니다. 자동화에는 `hamn upgrade --output json`이나
`hamn --headless system upgrade --yes`를 사용합니다. TUI를 정상 종료한 뒤 새
릴리스를 안내할 수 있지만 스스로 설치하지는 않습니다. 이 확인은
`HAMN_NO_UPDATE_CHECK=1`로 끌 수 있습니다.
