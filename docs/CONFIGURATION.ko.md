# 설정

이 문서는 Hamn 0.0.1 설정 reference의 한국어 번역입니다. 기준 영문 문서는
[CONFIGURATION.md](CONFIGURATION.md)입니다.

## 프로필 선택 및 위치

프로필 상태는 `~/.hamn/<profile>/`에 저장하며 디렉터리 권한은 `0700`입니다.
헤드리스 VM 작업에는 명시적 `--profile`이 필요하고 `vm list`만 예외입니다.
Docker는 `--profile` 또는 외부 `--context` 중 정확히 하나를 지정합니다.
`--docker-config`는 context를 사용하는 Docker 요청에만 적용합니다.
TUI는 초기값 `default`인 자체 선택을 유지합니다. 공개 API에는 위치 인자 프로필이나
`HAMN_PROFILE` 대체 선택이 없습니다.

프로필 이름에는 영문자, 숫자, _, -만 쓸 수 있습니다. cache, ., ..은 유효한
프로필 이름이 아닙니다.

로그는 `~/.hamn/<profile>/logs/`에 있습니다. `serial.log`는 현재 부팅의 게스트
콘솔이고, 시작할 때마다 직전 부팅의 콘솔을 `serial.previous.log`로 남깁니다. 그래서
실패한 시작의 콘솔은 한 번의 재시도까지 보존됩니다. `vmrun.log`와
`port-observer.log`는 이어 쓰는 파일이며, Hamn이 쓰는 줄은 UTC 시각으로 시작하고 SSH
클라이언트가 쓰는 줄에는 시각이 없습니다.

`vm delete`는 VM을 정지하고 프로필을 목록에서 빼지만 `~/.hamn/<profile>/`과 그 안의
`disk.img`, Docker 데이터는 남깁니다. 그 뒤 같은 이름의 `vm create`는
`profile already exists`로 거절됩니다. 프로필 하나의 디스크만 버리는 연산은 없고
`system uninstall`은 모든 프로필과 디스크를 지웁니다. 빈 디스크에서 시작하려면 새
이름으로 프로필을 만드세요.

`~/.hamn/<profile>/config.yaml`은 프로필 설정 파일입니다. TUI 기본 작업 영역은
별도 `~/.hamn/tui.json`에 저장합니다. [TUI 환경설정](TUI.ko.md)을 참고하세요. 이 파일은 mode 0600으로
atomic write됩니다. runtime=containerd 또는 runtime=hamn이 있는 legacy hamn.conf는
fail closed합니다. Hamn은 legacy runtime data를 제자리에서 변환하지 않습니다.

## 설정 편집

```sh
hamn --headless vm create --profile work --cpu 4 --memory 4 --yes
hamn --headless vm configure --profile work --cpu 6 --memory 8 --disk 80 --yes
hamn --headless vm configure --profile work --rosetta true --yes
hamn --headless vm start --profile work --yes
```

`configure`는 정지된 프로필만 변경하며 기존 VM 디스크를 축소하지 않습니다.
`create`와 `configure`는 `--rosetta <true|false>`도 받습니다. 그 밖의 고급 설정은 VM이
정지된 상태에서 `~/.hamn/<profile>/config.yaml`을 편집하세요. 설정 변경은 지정하지 않은
항목(마운트·Docker daemon 설정·Rosetta·provisioning hook)을 보존합니다. TUI의 `v` → `c`로 리소스 설정 명령을 편집할 수 있습니다.
네이티브 `kubectl edit`는 내부 터미널에서 설치된 편집기를 실행합니다.

## YAML schema

다음은 완전한 기본 template입니다.

~~~yaml
cpus: 4
memoryMiB: 4096
diskGiB: 60
mountHome: true
homeReadOnly: false
mountInotify: false
docker:
  daemonJson: ""
rosetta: false
nestedVirtualization: false
sshAgent: false
mounts: []
provision: []
~~~

Parser는 정확히 하나의 YAML document만 허용합니다. Duplicate 또는 unknown key,
alias, anchor, tag, merge key, plain이 아닌 boolean/integer, 잘못된 collection type,
잘못된 path를 거부합니다. YAML implicit type coercion에 의존하지 마세요.

| Key | Type 및 기본값 | 의미 |
| --- | --- | --- |
| cpus | 양의 정수, 4 | VM CPU 개수 |
| memoryMiB | 양의 정수, 4096 | MiB 단위 VM memory |
| diskGiB | 양의 정수, 60 | Guest disk 용량. 확장만 가능 |
| mountHome | boolean, true | 사용자 home directory를 virtiofs로 노출 |
| homeReadOnly | boolean, false | home share를 read-only로 설정. mountHome이 false면 유효하지 않음 |
| mountInotify | boolean, false | writable virtiofs share의 기존 file만 대상으로 하는 실험적 best-effort bridge. writable share가 하나 이상 필요 |
| docker.daemonJson | JSON object 하나를 담은 string, 빈 string | Hamn 관리 경계를 바꾸지 않는 Docker daemon 설정 |
| rosetta | boolean, false | Host가 지원할 때 Apple Linux Rosetta translation 요청 |
| nestedVirtualization | boolean, false | macOS 15 이상, M3 칩 이상 Mac 및 framework capability check가 지원할 때 nested virtualization 요청 |
| sshAgent | boolean, false | Hamn SSH session에만 사용자의 SSH agent forward |
| mounts | 최대 16개 sequence | 추가 virtiofs share |
| provision | 최대 16개 sequence | Lifecycle hook |

`rosetta`를 켜지 않으면 `linux/amd64` container는 guest의 QEMU user-mode emulation으로
실행됩니다. 단순한 프로그램은 동작하고 `uname -m`도 `x86_64`를 출력하지만, 그것이
특정 workload가 동작한다는 근거는 아닙니다. 예를 들어 Node.js `corepack`은
`QEMU internal SIGSEGV`(종료 코드 139)로 끝납니다. VM 상태의 `rosetta`는 설정값이며
실제로 쓰이는 번역기를 나타내지 않습니다. amd64 build와 runtime이 필요하면 VM을
정지하고 `hamn --headless vm configure --profile <name> --rosetta true --yes`를 실행한
뒤(또는 `config.yaml`의 `rosetta`를 `true`로 바꾼 뒤) 다시 시작하세요.

## Docker daemon JSON

docker.daemonJson은 duplicate key가 없는 strict JSON object여야 합니다. Hamn이
Docker와 network 설정을 예약한 뒤 guest /etc/docker/daemon.json에 merge합니다.
사용자가 다음 managed boundary를 바꾸려 하면 Hamn은 거부하거나 guest transaction을
실패시킵니다.

~~~text
containerd, host-gateway-ip, hosts, data-root, exec-root,
dns, bip, bridge, fixed-cidr, default-address-pools
~~~

features.buildkit을 설정하면 반드시 true여야 하며 Hamn은 BuildKit을 활성 상태로
유지합니다. Guest system containerd socket, Docker bridge DNS,
host.docker.internal gateway는 항상 Hamn이 설정합니다. 설정 오류는 partial daemon을
조용히 수용하는 대신 이전 guest transaction을 복구 가능한 상태로 둡니다.

그 밖의 daemon 설정은 그대로 전달됩니다. Registry mirror가 한 예입니다. 한 VM의 모든
pull은 하나의 주소로 Docker Hub에 도달하고, Docker Hub는 익명 pull을 주소별로
제한합니다.

~~~yaml
docker:
  daemonJson: "{\"registry-mirrors\":[\"https://mirror.gcr.io\"]}"
~~~

다음 시작 뒤 `docker info`의 `Registry Mirrors`에 mirror가 표시됩니다.

## Mount

추가 mount schema는 다음과 같습니다.

~~~yaml
mounts:
  - location: "/Users/<your-user>/project"
    mountPoint: "/workspace/project"
    writable: true
  - location: "/Volumes/reference-data"
    mountPoint: "/reference-data"
    writable: false
~~~

location과 mountPoint는 필수 absolute normalized path입니다. writable 기본값은
false이며 mountPoint는 서로 달라야 합니다. Launch 전에 Hamn은 host source가
사용자 소유 non-symlink directory인지 검사합니다. Writable source는 canonical
$HOME 안에 있어야 하고 $HOME 밖 source는 read-only여야 합니다. sshAgent를
활성화해도 SSH agent socket을 container에 자동 mount하지 않습니다.

Container가 bind mount할 수 있는 host path는 VM이 공유하는 경로뿐입니다.
`mountHome`이 true일 때의 `$HOME`과 각 `mounts` 항목의 `mountPoint`입니다.
`hamn --headless vm status --profile <name>`의 `sharedDirectories`에서 확인할 수 있습니다.
Docker daemon은 bind source를 guest 안에서 찾고, Hamn은 외부 Docker 도구가
프로필 소켓으로 보내는 요청을 보지 못하므로 공유 밖 경로를 거절하지 않습니다.

- `docker run -v /private/tmp/data:/data`는 빈 `/data`로 성공합니다. Daemon이
  없는 source directory를 guest에 만들기 때문이며, host file을 하나도 읽지
  않은 명령이 종료 코드 0으로 끝날 수 있습니다.
- `docker run --mount type=bind,source=/private/tmp/data,target=/data`는
  `bind source path does not exist`로 실패합니다. 다만 앞선 `-v`가 그
  directory를 guest에 이미 만들었다면 실패하지 않습니다.

`$HOME` 밖 directory를 쓰려면 Docker에 넘기는 path를 `mountPoint`로 하는
read-only mount로 추가하세요. `location`은 canonical path여야 합니다. 예를 들어
symlink인 `/tmp`가 아니라 `/private/tmp/...`입니다.

~~~yaml
mounts:
  - location: "/private/tmp/build-input"
    mountPoint: "/tmp/build-input"
    writable: false
~~~

`mountInotify`는 기본적으로 꺼져 있습니다. 켜면 Hamn은 macOS FSEvents로 writable host
share를 감시하고 guest agent에게 대응하는 기존 regular file의 timestamp 갱신을 요청합니다.
file content를 다시 쓰지 않고 Linux `IN_ATTRIB`와 `IN_CLOSE_WRITE` event를 만듭니다. 새 file,
삭제·rename path, directory, symlink, drop/coalesce된 FSEvents record의 event는 보장하지
않습니다. Agent는 path traversal, read-only share, symlink, regular file이 아닌 대상을
거부합니다. 일반 virtiofs write만 보장된 host-to-guest 파일 변경 동작입니다.

## Network

모든 profile은 Virtualization.framework shared NAT를 사용합니다. Guest는 private
address를 가지며 published TCP port에는 SSH ControlMaster, published UDP port에는
bounded host process를 사용합니다. Setup이 중단되면 forwarding state를
transactionally repair합니다. Network attachment는 profile 설정이 아닙니다. Strict
YAML schema는 `network`를 거부하고 `configure`에는 `--network` 또는
`--network-interface` option이 없습니다. Hamn은 0.0.1에서 LAN-reachable guest
address를 제공하지 않습니다.

Published port는 Docker daemon이 container를 받아들인 뒤에 프로필별 port observer가
전달합니다. 그래서 다음 세 경우에는 `docker run`이 실패하지 않습니다.

- UDP port를 `-p 127.0.0.1:5353:5353/udp`처럼 한 주소에 publish한 경우. Guest의
  Docker는 그 port를 guest의 그 주소에서만 받습니다. `127.0.0.1`이면 guest 자신의
  loopback이며, host relay는 그곳으로 보낼 수 없습니다. Hamn은 이런 port에 relay도
  host listener도 만들지 않습니다. VM 상태의 `portForwardFailures`에 그 port가
  `udpAddressUnsupported`로 나타나고, `~/.hamn/<profile>/logs/port-observer.log`에
  `cannot forward published udp port <address>:<port>: a UDP port is forwarded only when it is published on all addresses`가
  한 번 기록됩니다. 전달되게 하려면 모든 주소에 publish하세요(`-p 5353:5353/udp`).
  이때 host relay는 Mac의 모든 주소에서 listen하며, Mac의 loopback에서만 listen하는
  UDP 전달은 Hamn에 없습니다. 이 한계는 TCP에는 해당하지 않습니다.
  `-p 127.0.0.1:8080:80`으로 publish한 port는 전달됩니다.
- Host process가 published port를 이미 listen하고 있는 경우. `docker ps`에는 mapping이
  보이지만 host port로 들어온 연결은 계속 그 process가 받습니다. VM 상태의
  `portForwardFailures`에 그 port가 `hostPortInUse`로 나타나고,
  `~/.hamn/<profile>/logs/port-observer.log`에
  `cannot forward published tcp port <address>:<port>: another process holds the host port`가
  한 번 기록됩니다. Observer는 계속 재시도하며 port가 비면 전달이 시작됩니다.
- Observer는 동기화마다 실행 중인 container 목록을 한 번 읽습니다. Container가 2,048개를
  넘거나, 목록 응답이 512 KiB 이상이거나, published port mapping이 128개를 넘으면
  한도 아래로 돌아올 때까지 모든 published port의 동기화가 멈춥니다. Log에는 그 사실이
  한 번 기록되고(`the container list exceeds the port observer's limits`), 이 경우
  `portForwardFailures`에는 port가 나타나지 않습니다. 이 한도는 observer의 것이며
  헤드리스 `docker containers list`의 한도가 아닙니다.

Observer는 VM 실행 중에 끊긴 전달도 복구합니다. Published UDP port의 host relay
process가 끝났으면 다음 동기화에서 새 relay를 시작합니다. 실행 중인 VM에
`hamn start`로 다시 연결한 뒤에는, 새 SSH 연결에 이전 forward가 하나도 없으므로
observer가 모든 published TCP port를 다시 요청합니다. Relay를 확인할 수 없는 UDP
port는 그대로 두고 `portForwardFailures`에 `forwardFailed`로 표시합니다.

Guest Docker network는 host.docker.internal을 resolve합니다. 0.0.1의
host.hamn.internal alias는 제거되었으므로 host.docker.internal을 사용합니다. Hamn은 host
/var/run/docker.sock을 건드리지 않습니다.

## Kubernetes

Kubernetes는 Hamn VM과 독립적인 외부 kubeconfig context를 사용합니다.

```sh
hamn --headless k8s contexts list
hamn --headless k8s pods list --context dev --namespace default
```

`--kubeconfig`, `KUBECONFIG`, 기본 `~/.kube/config` 순서로 설정을 선택하며 원본 파일을
바꾸지 않습니다. 인증과 변경 대상 지정은 [API](API.ko.md)를 참고하세요.
매니지드 K3s용 `kubernetes` 항목은 제거되었습니다. 이 항목이 남은 프로필은 알 수
없는 설정 키로 거부합니다. 프로필을 사용하려면 `config.yaml`에서 해당 항목을
삭제하세요. 제거된 릴리스의 K3s 데이터는 이전하지 않습니다.

## Provisioning hook

각 hook은 stage, command, 선택 timeoutSeconds, 선택 mode를 가집니다.

~~~yaml
provision:
  - stage: "system"
    command: "apt-get update"
    timeoutSeconds: 120
    mode: fail
  - stage: "ready"
    command: "echo application-ready"
    timeoutSeconds: 30
    mode: warn
~~~

유효한 stage 실행 순서는 system, user, after-boot, ready입니다. system, after-boot,
ready는 guest root 권한으로 실행하고 user는 guest hamn user로 실행합니다. Timeout은
1–3600초여야 하며 기본값은 60초입니다. fail이 기본이며 startup을 중지합니다.
warn은 failure를 기록하고 계속합니다. Log에는 hook command/output 대신 redacted
metadata만 남습니다.

## Docker context와 SDK 환경

Hamn은 외부 Docker context를 생성·활성화·복원하지 않습니다.
`hamn --headless vm env --profile work`에서 접속 정보를 읽거나 프로필 소켓을
직접 지정하세요.

```sh
export DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock"
export TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE=/var/run/docker.sock
export TESTCONTAINERS_HOST_OVERRIDE=host.docker.internal
```

Docker CLI·Compose·buildx·SDK는 같은 Docker 소켓을 사용합니다.
공개 containerd 소켓은 제공하지 않습니다.

`docker cp -`는 tar stream에 실린 확장 속성을 그대로 적용하는데, guest 파일시스템은
macOS 속성을 받지 않습니다. macOS `tar`로 만든 stream은
`lsetxattr ...: xattr "com.apple.provenance": operation not supported`로 실패합니다.
속성 없이 stream을 만드세요.

```sh
COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata -cf - dir | docker cp - container:/
```


## 업그레이드 확인과 파일 캐시

대화형 TUI가 성공적으로 종료되면 캐시된 새 버전 정보를 보여주고 백그라운드에서
메타데이터 확인 하나를 예약할 수 있습니다. 성공 캐시는 24시간, 실패 재시도 간격은
6시간이며 동일 버전 안내는 24시간에 한 번입니다. headless·내부 모드, 비터미널 출력,
CI, `HAMN_NO_UPDATE_CHECK=1`에서는 실행하지 않습니다. 자동 설치나 telemetry는 없습니다.
전용 메타데이터는 `~/.hamn/cache/update-check-v1.json`, `update-notice-v1.json`에
저장합니다. `~/.hamn/cache/downloads`의 파일은 SHA-256 기준으로 관리하며 크기·digest를
검증한 뒤에만 재사용·게시합니다. 수동 `upgrade --check`·`--force`, 복구와
`--headless system upgrade --yes`는 [설치](INSTALLATION.ko.md)를 참고하세요.
