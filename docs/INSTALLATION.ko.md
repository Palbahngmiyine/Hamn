# 설치와 업데이트 경험

영문은 [INSTALLATION.md](INSTALLATION.md)를 참고하세요.

처음 설치하거나 이전 업데이트기가 공개 manifest를 읽지 못하면
[공식 설치기](../README.ko.md#설치)를 사용합니다. 관리형 설치에서는 다음 명령을 실행합니다.

```sh
hamn --headless system update --help
hamn --headless system update --yes
hamn --version
```

업데이트는 설치된 버전과 선택된 버전, 다운로드·검증 단계, 완료 후 버전 요약을
표시합니다. 대화형 headless 터미널의 stderr에는 다운로드 진행률을 표시하고,
리다이렉션한 stderr와 TUI 로그에는 단계별 일반 텍스트를 표시합니다.
headless stdout은 JSON 결과를 유지합니다. 최초 설치기는 PATH 안내를 위해서만
호출자의 PATH를 보존하며, 설치에는 고정된 시스템 도구 PATH를 사용합니다.
`~/.local/bin`이 PATH에 없을 때만 셸 설정 명령을 안내합니다.

호스트·게스트 SHA-256 검증을 마친 뒤 설치하며, 압축을 푼 호스트의 버전도
manifest와 일치해야 합니다. 바이너리와 새 프로필용 이미지 선택은 기존 복구
저널을 통해 반영합니다. VM을 재시작하거나 기존 프로필 디스크를 교체하지 않습니다.
선택된 이미지는 새 프로필 디스크에 적용됩니다. 바이너리 롤백으로 게스트 상태나
기존 K3s 데이터 정리를 되돌릴 수는 없습니다.

실패하면 진단 메시지를 확인한 뒤 재시도합니다. manifest 옵션을 지정했다면 같은
옵션을 유지합니다. 새 업데이트를 시작하기 전에 중단된 트랜잭션을 복구합니다.
복구 자체가 실패할 수도 있으므로, 복구 성공을 확인하지 않고 이전 바이너리가
활성 상태라고 단정하지 않습니다. 메타데이터 호환성 오류에는 공식 설치기 링크도
표시합니다. 로컬 설치 성공은 물리 VM 검증이 아니며 최초 설치 메타데이터에는
`github-hosted-no-vm`을 기록합니다.

## 호환성 계약

schema v2 발행 필드는 `schemaVersion`, `channel`, `version`, `commit`,
`validationMode`, `compatibility`, `artifacts`로 유지합니다. 이전에 공개된
v0.1.x manifest의 `repository`는 선택적인 `owner/name` 설명 정보로 허용하며,
그 외 알 수 없는 필드와 잘못된 값은 거부합니다. 저장소 정보가 아티팩트 검증을
대체하지 않습니다. 발행 측 필드를 기존 형식으로 유지해 이전의 엄격한 v2
업데이트기도 향후 릴리스를 읽게 합니다. 이미 공개된 릴리스는 수정하지 않습니다.

## 레퍼런스 분석

Microsoft·GitHub·Rust 재단 생태계의 공식 프로젝트로 대상을 제한했습니다.
상호작용 설계 참고이며 사용성을 측정해 순위를 매긴 결과는 아닙니다.

| 공식 프로젝트 | 확인한 패턴 | Hamn 적용 |
| --- | --- | --- |
| [GitHub CLI](https://cli.github.com/manual/gh_extension_upgrade) | 작업별 사용법·옵션·dry-run 설명, [stderr 업데이트 알림](https://cli.github.com/manual/gh_help_environment) | 작업별 도움말, 필수 `--yes` 설명, JSON과 진행 안내 분리 |
| [Microsoft .NET 설치기](https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-install-script) | [소스](https://github.com/dotnet/install-scripts/blob/47940ac9fc30a2f2dd19167165d0bb0774625f67/src/dotnet-install.sh)의 다운로드·압축 해제·설치 버전·PATH 안내, dry-run의 재실행 명령 | 단계 안내, 재시도 방법, 필요한 경우의 PATH 안내 |
| [rustup](https://rust-lang.github.io/rustup/installation/) | [버전 요약 구현](https://github.com/rust-lang/rustup/blob/454ff04cdefebc8f38f47f64b3904866f9e0660f/src/cli/common.rs)의 설치·업데이트·변경 없음·실패 구분 | 설치 전후 버전 표시, 반영 완료 후 성공 요약 |

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
기존 업데이트 테스트는 중단과 롤백을 검사합니다. 제어된 전송은 진행률 모드
선택의 검증이며 실제 네트워크 처리량이나 VM 부팅 검증은 아닙니다.
