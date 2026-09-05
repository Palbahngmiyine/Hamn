# 설정

이 문서는 Hamn 0.0.1 설정 reference의 한국어 번역입니다. 기준 영문 문서는
[CONFIGURATION.md](CONFIGURATION.md)입니다.

## 프로필 선택 및 위치

프로필 상태는 `~/.hamn/<profile>/`에 저장하며 디렉터리 권한은 `0700`입니다.
헤드리스 VM·Docker 작업에는 명시적 `--profile`이 필요하고 `vm list`만 예외입니다.
TUI는 초기값 `default`인 자체 선택을 유지합니다. 공개 API에는 위치 인자 프로필이나
`HAMN_PROFILE` 대체 선택이 없습니다.

프로필 이름에는 영문자, 숫자, _, -만 쓸 수 있습니다. cache, ., ..은 유효한
프로필 이름이 아닙니다.

~/.hamn/<profile>/config.yaml만 지원되는 설정 파일입니다. 이 파일은 mode 0600으로
atomic write됩니다. runtime=containerd 또는 runtime=hamn이 있는 legacy hamn.conf는
fail closed합니다. Hamn은 legacy runtime data를 제자리에서 변환하지 않습니다.

## 설정 편집

```sh
hamn --headless vm create --profile work --cpu 4 --memory 4 --yes
hamn --headless vm configure --profile work --cpu 6 --memory 8 --disk 80 --yes
hamn --headless vm start --profile work --yes
```

`configure`는 정지된 프로필만 변경하며 기존 VM 디스크를 축소하지 않습니다.
고급 설정은 VM이 정지된 상태에서 `~/.hamn/<profile>/config.yaml`을 편집하세요.
리소스 설정 변경은 기존 마운트·Docker daemon 설정·Rosetta·provisioning hook을
보존합니다. TUI는 임의 셸 명령이나 외부 편집기를 실행하지 않습니다.

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

Guest Docker network는 host.docker.internal을 resolve합니다.
host.hamn.internal은 0.0.1 compatibility alias입니다. Guest Docker configuration이
성공하면 다음 release에서 제거된다는 warning을 출력합니다. Hamn은 host
/var/run/docker.sock을 건드리지 않습니다.

## Kubernetes

Kubernetes는 Hamn VM과 독립적인 외부 kubeconfig context를 사용합니다.

```sh
hamn --headless k8s contexts list
hamn --headless k8s pods list --context dev --namespace default
```

`--kubeconfig`, `KUBECONFIG`, 기본 `~/.kube/config` 순서로 설정을 선택하며 원본 파일을
바꾸지 않습니다. 인증과 변경 대상 지정은 [API](API.ko.md)를 참고하세요.
구형 YAML의 `kubernetes` 항목은 K3s 전환을 위해서만 읽고 정리 성공 후 제거합니다.
신규 프로필 설정이 아닙니다. K3s 클러스터 데이터·전용 볼륨은 삭제하고
Docker 데이터·원본 kubeconfig는 보존합니다.

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
