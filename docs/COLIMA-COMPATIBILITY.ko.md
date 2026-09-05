# Colima 공존과 이전

영문 기준 문서는 [COLIMA-COMPATIBILITY.md](COLIMA-COMPATIBILITY.md)입니다.
Hamn은 자체 Apple Silicon VM과 외부 kubeconfig 클러스터를 관리합니다.
Colima의 프로필·디스크·소켓·설정을 재사용하지 않습니다.

## 작업

`hamn`은 TUI를 엽니다. 자동화에는 다음 인터페이스를 사용합니다.

| 목적 | Hamn 명령 |
| --- | --- |
| 프로필 시작 | `hamn --headless vm start --profile work --yes` |
| 프로필 정지 | `hamn --headless vm stop --profile work --yes` |
| 활성 목록에서 제거하고 디스크 보존 | `hamn --headless vm delete --profile work --yes` |
| 프로필 목록 | `hamn --headless vm list` |
| 상태 조회 | `hamn --headless vm status --profile work` |
| 정지한 프로필 설정 | `hamn --headless vm configure --profile work --cpu 6 --memory 8 --disk 80 --yes` |
| Docker 연결 정보 | `hamn --headless vm env --profile work` |
| 컨테이너 목록 | `hamn --headless docker containers list --profile work` |
| 외부 Kubernetes context 목록 | `hamn --headless k8s contexts list` |
| 외부 Pod 목록 | `hamn --headless k8s pods list --context dev --namespace default` |
| Hamn과 관리 데이터 전체 제거 | `hamn --headless system uninstall --yes` |

메모리와 디스크 단위는 GiB입니다. `vm env`를 포함해 결과는 JSON이므로
`eval`에 전달하지 않습니다. 기존 CLI 문법과 JSON 형식은 호환되지 않습니다.
내장 인터페이스에는 매니지드 K3s 시작, 임의 게스트 셸, 컨테이너 생성, Compose
실행, Kubernetes apply·exec가 없습니다. 외부 도구는 Docker Engine API를
계속 사용할 수 있습니다.

## 외부 Docker 도구

VM 부팅과 내장 컨테이너 관리에는 Docker CLI가 필요하지 않습니다. Hamn은
`docker context`를 전환하거나 정지할 때 이전 context를 복구하지 않습니다.
외부 도구의 소켓을 명시적으로 선택합니다.

```sh
hamn --headless vm start --profile work --yes
DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock" docker ps
DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock" docker compose up -d
DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock" docker buildx build --load -t example .
```

공개 접속점은 `/var/run/docker.sock`이 아니라 프로필 소켓입니다. Docker는
게스트 containerd의 `moby`를 사용합니다. 게스트 containerd·CRI 소켓은 호스트
공개 API가 아닙니다. 네트워크는 공유 NAT와 소유권을 관리하는 TCP/UDP 포워딩을
사용합니다. [아키텍처](ARCHITECTURE.ko.md)를 참고하세요.

## 이전 범위

Colima의 VM·Docker context·소켓·설치·상태를 보존합니다. Hamn을 별도로 설치하고
이름 있는 프로필을 선택한 뒤 의도한 Docker 명령이나 SDK만 해당 소켓으로
연결합니다. 애플리케이션의 Compose·buildx·SDK 동작을 확인하고 Hamn 정지 후
Colima 상태가 동일한지 확인합니다. Docker 객체를 Colima에서 자동 복사하지 않습니다.

구형 Hamn 업데이트에는 별도의 자동 K3s 전환이 적용됩니다. K3s 클러스터 데이터와
전용 로컬 볼륨은 영구 삭제하며 Docker 객체·이름 있는 볼륨·사용자 마운트는
보존합니다. 원본 kubeconfig 파일은 보존하고 기존 Hamn 소유 로컬 context는
사용 불가로 표시합니다. 바이너리를 롤백해도 K3s 데이터는 복구되지 않습니다.
전환 시점과 실패 처리는 [설정](CONFIGURATION.ko.md)을 참고하세요.
