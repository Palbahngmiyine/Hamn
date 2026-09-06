# Hamn

Hamn은 하나의 macOS 실행 파일로 Linux VM, VM의 Docker Engine, 외부 Kubernetes
클러스터를 관리합니다. `hamn`은 Ratatui 터미널 화면을 열고,
`hamn --headless`는 자동화를 위한 JSON·NDJSON을 출력합니다.

## 요구 사항

- macOS 13 이상의 Apple Silicon Mac.
- VM·Docker 기능에는 서명된 Hamn 게스트 이미지가 필요합니다.
- Kubernetes에는 kubeconfig가 필요하며 Hamn VM과 독립적으로 사용할 수 있습니다.

TUI 컨테이너 탐색에는 Docker CLI, Kubernetes 탐색에는 kubectl이 필요합니다.
헤드리스 SDK 작업은 이 CLI들을 요구하지 않습니다. Compose·buildx·플러그인은
외부 설치 의존성이며 Docker Desktop은 필요하지 않습니다.

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

처음에 컨테이너 또는 Kubernetes를 선택합니다. Tab으로 영역을 전환하고 `,`로
기본 영역을 변경합니다. `:ps`, `:docker ps -a`, `:images`, `:get pods -A`,
`:kubectl get deployments -n dev`를 입력할 수 있습니다. 일반 목록은 선택 가능한
표로 표시하고 나머지는 내부 터미널에서 원래 CLI 의미대로 실행합니다.
`e`는 환경·context 선택, `v`는 Hamn 프로필의 VM 제어입니다.
작업·설정·취소는 [작업 영역과 명령 안내](docs/TUI.ko.md)를 참고하세요.

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

UI 선택은 Docker의 현재 context나 kubeconfig의 `current-context`를 바꾸지 않습니다.
명시적 `docker context use`·`kubectl config`는 원래 설정 변경 의미를 유지합니다.
헤드리스 인증은 비대화형이며, 네이티브 CLI 인증은 내부 터미널에서 해당 CLI 규칙을 따릅니다.

## 기존 설치 전환과 데이터

이번 변경은 기존 CLI·JSON 형식을 대체하고 매니지드 K3s를 제거합니다.
TUI 진입은 정리를 실행하지 않습니다. 구형 프로필은 다음 VM·Docker 변경 작업에서
정리하며, 정지된 프로필은 다음 시작 때 정리할 수 있습니다. 조회는 대기 상태만 표시합니다.

**K3s 클러스터 데이터와 전용 로컬 볼륨은 영구 삭제됩니다.** 실행 파일을 이전 버전으로
되돌려도 복구되지 않습니다. Docker의 `moby` 네임스페이스, Docker 볼륨, 공용
containerd content 저장소, 사용자 마운트, 원본 kubeconfig는 보존합니다.
중단된 전환은 기록된 단계부터 재개하고 실패를 완료로 표시하지 않습니다.

`vm delete`는 VM을 멈추고 목록에서 숨기며 디스크·Docker 데이터를 보존합니다.
`system uninstall --yes`는 모든 Hamn 프로필과 관리 설치 파일을 영구 삭제합니다.
저장 형식은 [설정](docs/CONFIGURATION.ko.md)을 참고하세요.

컨테이너 생성·Compose·exec·Kubernetes apply·port-forward는 TUI에서 외부 CLI로
실행하며 헤드리스 SDK 작업 집합에는 추가하지 않습니다. MCP 서버는 제공하지 않습니다.
