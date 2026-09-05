# Hamn

Hamn은 하나의 macOS 실행 파일로 Linux VM, VM의 Docker Engine, 외부 Kubernetes
클러스터를 관리합니다. `hamn`은 Ratatui 터미널 화면을 열고,
`hamn --headless`는 자동화를 위한 JSON·NDJSON을 출력합니다.

## 요구 사항

- macOS 13 이상의 Apple Silicon Mac.
- VM·Docker 기능에는 서명된 Hamn 게스트 이미지가 필요합니다.
- Kubernetes에는 kubeconfig가 필요하며 Hamn VM과 독립적으로 사용할 수 있습니다.

내장 Docker 관리에는 Docker CLI나 Docker Desktop이 필요하지 않습니다.
외부 Docker CLI·Compose·buildx·SDK는 프로필 소켓을 사용할 수 있습니다.

## 설치

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

```sh
hamn
```

소스 빌드는 [개발 문서](docs/DEVELOPMENT.ko.md)를 참고하세요.
서명된 릴리스 설치는 [릴리스 설정](docs/RELEASE-SETUP.ko.md)을 참고하세요.

## 터미널 화면

`:vm`, `:containers`, `:images`, `:volumes`, `:networks`, `:contexts`, `:ns`,
`:pods`로 화면을 선택합니다. `/`는 검색, 방향키·`j/k`는 이동, Enter는 상세,
Esc는 복귀, `?`는 도움말입니다. 선택한 프로필·context·namespace는 상단에
표시됩니다. 변경 작업은 확인을 거치며 화면을 닫아도 VM은 계속 실행됩니다.

## 헤드리스 인터페이스

```sh
hamn --headless capabilities
hamn --headless vm create --profile work --cpu 4 --memory 4 --yes
hamn --headless vm start --profile work --yes
hamn --headless docker containers list --profile work
hamn --headless docker containers logs api --profile work --follow
hamn --headless k8s contexts list
hamn --headless k8s pods list --context dev --namespace default
hamn --headless k8s deployments scale api --replicas 3 --context dev --namespace default --yes
hamn --headless vm stop --profile work --yes
```

변경에는 명시적 대상과 `--yes`가 필요합니다. 일회성 작업은 JSON 응답 하나,
`--follow` 로그와 `--watch` 조회는 NDJSON을 출력합니다. stdout에는 기계용 응답만
출력합니다. 자세한 계약은 [API](docs/API.ko.md)를 참고하세요.

## 외부 도구 연결

```sh
export DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock"
docker compose up -d
docker buildx build --load -t example .
```

Hamn은 Docker의 현재 context나 kubeconfig의 `current-context`를 바꾸지 않습니다.
Kubernetes 설정은 `--kubeconfig`, `KUBECONFIG`, `~/.kube/config` 순서로 선택하고,
명시한 context만 사용합니다. 대화형 인증이 필요하면 Hamn 밖에서 먼저 인증하세요.

## 기존 설치 전환과 데이터

이번 변경은 기존 CLI·JSON 형식을 대체하고 매니지드 K3s를 제거합니다.
첫 TUI 실행에서 실행 중인 구형 프로필을 정리하고, 정지된 프로필은 다음 시작 때
정리합니다. VM·Docker 변경 작업도 전환을 선행하며, 조회는 전환 대기 상태만 표시합니다.

**K3s 클러스터 데이터와 전용 로컬 볼륨은 영구 삭제됩니다.** 실행 파일을 이전 버전으로
되돌려도 복구되지 않습니다. Docker의 `moby` 네임스페이스, Docker 볼륨, 공용
containerd content 저장소, 사용자 마운트, 원본 kubeconfig는 보존합니다.
중단된 전환은 기록된 단계부터 재개하고 실패를 완료로 표시하지 않습니다.

`vm delete`는 VM을 멈추고 목록에서 숨기며 디스크·Docker 데이터를 보존합니다.
`system uninstall --yes`는 모든 Hamn 프로필과 관리 설치 파일을 영구 삭제합니다.
저장 형식은 [설정](docs/CONFIGURATION.ko.md)을 참고하세요.

컨테이너 생성, Compose 실행, 임의 셸·exec, Kubernetes apply·port-forward,
MCP 서버는 내장 명령 범위에 포함하지 않습니다.
