# 설치와 업그레이드

영문은 [INSTALLATION.md](INSTALLATION.md)를 참고하세요.

처음 설치하거나 이전 업데이트기와 호환되지 않으면
[공식 설치기](../README.ko.md#설치)를 사용합니다. 관리형 설치에서는 다음 명령을 실행합니다.

```sh
hamn upgrade --check
hamn upgrade
hamn upgrade --force --output json
hamn --headless system update --yes
hamn --version
```

`hamn update`는 `hamn upgrade`의 별칭입니다. 사용자가 직접 실행하는 `upgrade`는
설치 요청이며 headless 변경에는 계속 `--yes`가 필요합니다. `--check`는 manifest만
조회해 `up-to-date`, `repair-required`, `update-available`, `ahead`를 반환합니다.
디렉터리 생성, 아티팩트 획득, 저널 복구와 설치 상태 변경은 하지 않습니다.
지원하지 않는 설치에서는 네트워크 요청 없이 `unsupported-install`을 반환합니다.
`--check`와 `--force`는 함께 쓸 수 없습니다. 변경 요청은 stable downgrade,
개발 버전, 소스 빌드, 외부 package manager 설치와 직접 실행한 generation
바이너리에서 거부합니다. 관리형 명령 심볼릭 링크로 실행해야 합니다.
`--manifest URL`로 HTTPS manifest를 지정할 수 있으며 로컬 경로는 테스트 전용입니다.

일반 출력에는 버전과 전송량을 표시합니다. `--output json`은 `schemaVersion`,
`currentVersion`, `latestVersion`, `status`, `downloadedBytes`, `resumedBytes`,
`reusedBytes`, 아티팩트별 `artifacts`, `completed`, `profileDisksChanged=false`를
포함하는 JSON 객체 하나를 출력합니다. headless는 기존 JSON envelope의 data에
이 결과를 담습니다. 단계 안내는 stderr에 표시합니다. `resumedBytes`는 유효한
Range 응답으로 받은 downloaded byte의 부분집합이므로 총 네트워크 전송량을
계산할 때 `downloadedBytes`에 다시 더하지 않습니다.

generation을 변경하기 전에 manifest, 플랫폼, v3 아티팩트 크기, SHA-256과
압축을 푼 호스트 버전을 검증합니다. 최초 URL과 모든 redirect는 HTTPS여야 합니다.
실행 시 digest 검증은 릴리스 파이프라인의 keyless attestation 검증과 다릅니다.
클라이언트가 서명을 검증한다고 표현하지 않습니다. 크기가 없는 v2 호환 경로는
전체 다운로드를 사용합니다.

설치 기록은 버전과 호스트·게스트 digest를 바이너리·스크립트·패키징 내용에 연결합니다.
기록과 이미지가 모두 정상인 동일 릴리스는 payload 요청 없이 끝납니다. 호스트가
정상이면 손상된 게스트 선택이나 이미지만 복구하며 바이너리를 교체하지 않습니다.
`--force`는 동일 버전의 호스트 재설치를 허용하며 검증된 아티팩트는 재사용합니다.
호스트 자체의 무결성이 손상됐으면 검증 후 재설치합니다. 버전 문자열만으로 재사용을
허용하지 않습니다.

다운로드는 SHA-256별로 `~/.hamn/cache/downloads/`에 보관하고 소유자 전용 lock으로
직렬화합니다. 안전한 partial file은 Range와 기록된 validator로 이어받습니다.
Range를 거부하거나 무시하면 전체 다운로드를 한 번 재시도합니다. 크기나 digest가
다른 파일은 게시하지 않습니다. 공식 설치기도 같은 획득 helper를 포함해 검증된
호스트 아카이브를 재사용합니다. 외부 telemetry를 전송하지 않습니다.
수동 전송은 요청마다 연결 15초, 전체 600초로 제한합니다. 네트워크 실패를 기존처럼
자동으로 3회 재시도하지 않습니다. 명령을 다시 실행하면 안전한 v3 partial을
이어받으며 v2는 전체 다운로드를 다시 시작합니다.

대화형 TUI가 정상 종료되면 캐시된 업데이트 안내를 표시하고 분리된 manifest
검사를 예약할 수 있습니다. TUI에서 네트워크를 기다리지 않으며 실행 중인 CLI
세션, headless와 내부 명령에서는 검사하지 않습니다. 성공 결과는 24시간,
실패 결과는 6시간 재사용하고 같은 버전 안내는 24시간에 한 번만 표시합니다.
stdout·stderr가 TTY인 관리형 stable 설치에서만 실행합니다. `CI` 또는
`HAMN_NO_UPDATE_CHECK=1`이면 자동 검사를 끕니다. 연결 제한은 2초,
전체 요청 제한은 5초입니다. 캐시는 `~/.hamn/cache/update-check-v1.json`과
`update-notice-v1.json`에 저장합니다.

업데이트기는 복구와 게시를 직렬화하고 durable journal을 사용합니다. v1·v2 저널을
복구하며 v2는 호스트 변경 여부를 기록해 게스트만 복구할 때 바이너리 포인터를
바꾸지 않습니다. 중단된 변경은 `--manifest`를 포함한 원래 옵션으로 다시 실행합니다.
복구가 실패하면 저널을 남겨 안전하지 않은 VM 시작을 막습니다. 업데이트는 VM을
재시작하거나 기존 프로필 디스크를 교체하지 않습니다. 바이너리 롤백으로 게스트
상태나 이미 정리한 legacy K3s 데이터를 복구할 수 없습니다. 첫 설치 실패에는
이전 바이너리가 없어 명령이 남을 수 있고 이미지 선택을 복구합니다.

최초 설치기는 고정된 시스템 도구 PATH를 사용하고 호출자의 PATH는 설정 안내에만
사용합니다. 앞에서 선택되는 다른 `hamn`을 알리며 PATH 변경 후 새 터미널을
열도록 안내합니다. 이전 형식의 일반 바이너리는 관리형 설치로 이전해야 합니다.
로컬 설치 통과는 물리 VM 검증이 아닙니다.

## Manifest 호환성과 릴리스 근거

publisher는 기존 schema v2 `hamn-update-manifest.json`을 유지하면서 동일한
호스트·게스트 digest를 가진 v3 `hamn-update-manifest-v3.json`도 생성합니다.
새 설치는 v3를 가리킵니다. v3는 정확한 아티팩트 크기, qcow2/zlib와 8 GiB
virtual size를 포함합니다. manifest는 256 KiB, 호스트 아카이브는 128 MiB 이하,
게스트 아티팩트는 2 GiB 미만으로 제한합니다. 중복·미지의 JSON key는 거부합니다.
v2 consumer는 이전에 발행된 올바른 `repository` 확장도 허용합니다.
이미 공개된 과거 릴리스는 수정하지 않습니다.

발행에는 실제 이미지 크기 근거와 검토된 `guest/image/release-size-budget.json`이
필요합니다. review-only report나 검토된 budget 부재는 발행을 차단합니다.
Hosted validation으로 물리 VM 동작을 검증했다고 주장하지 않습니다.

## 레퍼런스 분석

Microsoft·GitHub·Rust 재단 생태계의 공식 프로젝트로 대상을 제한했습니다.
상호작용 설계 참고이며 사용성을 측정해 순위를 매긴 결과는 아닙니다.

| 공식 프로젝트 | 확인한 패턴 | Hamn 적용 |
| --- | --- | --- |
| [GitHub CLI](https://cli.github.com/manual/gh_extension_upgrade) | 작업별 사용법·옵션·dry-run 설명, [stderr 업데이트 알림](https://cli.github.com/manual/gh_help_environment) | 작업별 도움말, 필수 `--yes` 설명, JSON과 진행 안내 분리 |
| [Microsoft .NET 설치기](https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-install-script) | [소스](https://github.com/dotnet/install-scripts/blob/47940ac9fc30a2f2dd19167165d0bb0774625f67/src/dotnet-install.sh)의 다운로드·압축 해제·설치 버전·PATH 안내, dry-run의 재실행 명령 | 단계 안내, 재시도 방법, 필요한 경우의 PATH 안내 |
| [rustup](https://rust-lang.github.io/rustup/installation/) | [버전 요약 구현](https://github.com/rust-lang/rustup/blob/454ff04cdefebc8f38f47f64b3904866f9e0660f/src/cli/common.rs)의 설치·업데이트·변경 없음·실패 구분 | 설치 전후 버전 표시, 검증된 변경 없음 결과, 반영 완료 후 성공 요약 |

PTY 터미널에서 GitHub CLI 2.100.0의 `extension upgrade --help`와
`--all --dry-run`, rustup 1.29.1의 `update --help`, Microsoft 설치기의 `--help`와
`--dry-run --version 8.0.100 --architecture arm64 --os osx`를 실행했습니다.
GitHub dry-run은 설치된 확장이 없다고 응답했고 .NET dry-run은 다운로드 URL과
재실행 명령을 출력했습니다. 참고 도구를 업데이트하지는 않았습니다.
위 다운로드·완료 패턴은 공식 소스도 확인한 결과이며, 참고 도구의 전체 설치를
직접 관찰했다는 뜻은 아닙니다.

Hamn 검증은 격리된 HOME에서 실제 실행 파일·worker·관리형 설치기를 사용합니다.
제어된 curl fixture가 진행 안내를 관찰할 때까지 대기한 뒤 체크섬이 고정된
아티팩트를 제공합니다. PTY와 리다이렉션 실행으로 완료 전 안내, JSON 분리,
버전 요약, 잘못된 메타데이터 거부 시 기존 상태 보존을 확인합니다.
업데이트 테스트는 공식 설치기 경로의 TERM·SIGKILL, 복구 후 재실패, 설치 기록 무효화,
동일 릴리스 반복과 PATH 충돌도 검사합니다. 제어된 전송은 출력 분리와 HTTP fixture의 실제 byte를 검증하며,
운영 네트워크 처리량이나 VM 부팅 검증은 아닙니다.
