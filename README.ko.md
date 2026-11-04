# Hamn

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/hamn-icon.svg">
  <img src="assets/branding/hamn-icon-on-light.svg" alt="Hamn" width="100">
</picture>

[English](README.md)

----

Hamn은 하나의 macOS 실행 파일로 Linux VM, VM의 Docker Engine, 외부 Kubernetes
클러스터를 관리합니다. `hamn`은 터미널 화면을 열고, `hamn --headless`는
자동화를 위한 JSON·NDJSON을 출력합니다.

----

## 요구 사항

- macOS 13 이상의 Apple Silicon Mac.
- 컨테이너 이미지는 `linux/arm64`용이어야 합니다. 게스트 이미지에는 amd64 에뮬레이션
  (Rosetta, qemu)이 없으므로 `linux/amd64` 이미지는 실행되지 않습니다.

TUI 컨테이너 탐색에는 Docker CLI, Kubernetes 탐색에는 kubectl이 필요합니다.
Docker Desktop은 필요하지 않습니다.

## 설치

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

설치기는 Hamn과 Linux 게스트 이미지를 내려받아 둘 다 검증한 뒤 `hamn` 명령을
`~/.local/bin`에 설치합니다. 이후 업데이트는 `hamn upgrade`로 합니다.

## 시작하기

```sh
hamn
hamn --headless vm create --profile work --cpu 4 --memory 4 --yes
hamn --headless vm start --profile work --yes
export DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock"
docker compose up -d
```

`hamn --headless capabilities`는 모든 헤드리스 작업을 보여 줍니다.

## 개발

소스에서 빌드하려면 macOS 13 이상의 Apple Silicon Mac, Apple 명령줄 개발자 도구,
`rust-toolchain.toml`의 Rust 툴체인이 필요합니다. 저장소 구조, 빌드 명령, 테스트
게이트는 [AGENTS.md](AGENTS.md)에 있습니다.

## 보안

취약점은 [SECURITY.md](SECURITY.md)에 적힌 방법으로 제보해 주세요.

## 라이선스

Hamn은 [MIT 라이선스](LICENSE)로 배포됩니다.
