# Hamn

Hamn은 Apple Silicon Mac에서 Linux VM으로 Docker workload를 실행합니다. 익숙한 Docker
CLI, Compose, buildx, SDK, Testcontainers와 선택형 K3s를 격리된 프로필별로 사용할 수
있습니다.

## 요구 사항

- macOS 13 이상을 실행하는 Apple Silicon Mac
- 별도로 설치한 Docker CLI. Docker Desktop은 필요하지 않습니다.

## 설치

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

실행 전에 bootstrap을 검증하려면
[Public release repository 설정](docs/RELEASE-SETUP.ko.md#6-설치)의 attestation 절차를
따르세요.

`~/.local/bin`이 `PATH`에 있는지 확인한 뒤 Hamn을 시작하고 Docker를 사용하세요.

```sh
hamn start
docker run --rm alpine uname -m
docker compose up -d
docker ps
```

Hamn은 활성 프로필의 Docker context를 자동으로 만듭니다. 시작한 뒤에는 평소처럼
Docker 명령을 사용하면 됩니다.

## 자주 쓰는 명령

```sh
hamn status
hamn stop
hamn start

docker compose down --volumes
docker buildx build --load -t example .
```

## 프로필

프로필은 VM, Docker socket, 설정, 데이터를 서로 분리합니다.

```sh
hamn start --profile work
hamn configure --profile work --cpu 6 --memory 8 --disk 80
hamn status --profile work

# Docker SDK와 Testcontainers
eval "$(hamn env --profile work)"
```

기본 설정은 `hamn template`으로 확인하고, 시작 전에 설정을 고치려면
`hamn start --edit`을 사용하세요.

## Kubernetes

K3s는 기본으로 꺼져 있습니다. 사용할 프로필에서 명시적으로 켜세요.

```sh
hamn start --profile dev
hamn kubernetes start --profile dev
hamn kubectl --profile dev -- get nodes
```

## 파일, 포트, 아키텍처 에뮬레이션

- 홈 디렉터리는 기본으로 VM에 mount됩니다. 추가 mount는 `hamn configure`로 설정합니다.
- Docker가 publish한 TCP·UDP port는 macOS에서 사용할 수 있습니다. Container에서는
  `host.docker.internal`로 Mac에 연결할 수 있습니다.
- amd64 container는 `binfmt`로 실행합니다. macOS가 지원하는 경우 Rosetta를 프로필별로
  선택할 수 있습니다.

## 프로필 제거

`hamn delete`는 프로필을 중지하고 나중에 다시 쓸 수 있도록 데이터를 보존합니다.
`hamn delete --data`는 확인 후 해당 프로필 데이터를 영구적으로 삭제합니다.

## 더 알아보기

- [설정](docs/CONFIGURATION.ko.md)
- [Colima 마이그레이션과 명령 대응](docs/COLIMA-COMPATIBILITY.ko.md)
- [보안 정책](docs/SECURITY.ko.md)
