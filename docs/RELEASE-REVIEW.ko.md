# 릴리스 후보 수동 검토

후보마다 배포 전에 작성합니다. 검토자, source commit/tree, 후보 manifest SHA-256,
정확한 바이너리 SHA-256, 날짜, 실행 명령, 증거 경로, 미해결 사항을 기록합니다.
체크하지 않은 항목은 통과가 아닙니다. [영문](RELEASE-REVIEW.md).

- [ ] 사용자 동작: 인자 없는 TUI·비TTY 오류, 헤드리스와 TUI 작업 동등성,
  profile/context/namespace 표시, 변경 영향 전체 확인, 한글·크기 변경·도움말·
  탐색·로그.
- [ ] 터미널·프로세스: Ctrl-C·SIGTERM·panic·suspend/resume 복구, TUI 종료 후 VM
  유지, 이전 응답의 대상 혼입 방지, 중단한 변경의 미확정 결과 표시.
- [ ] 보안: 단일 Mach-O·시스템 라이브러리 의존성·서명·예상 entitlement, 변조한
  이미지·출처 증거 거부, 프로필 경로·잠금·worker 프로토콜의 안전한 실패.
- [ ] API: Docker CLI 없는 내장 관리, 외부 소켓 연결, 외부 Kubernetes 인증·권한
  오류·감시 단절, 원본 kubeconfig 보존, 오래된 객체 식별자의 변경 거부.
- [ ] 성능: 시작 시간, 새로고침·로그 중 CPU/메모리, 네트워크 지연 중 입력 응답,
  스트림 제한의 측정값 기록.
- [ ] 격리·정리: 일회용 HOME에서 여러 프로필 격리, 작업 디렉터리 제거 전 소유한
  모든 프로필의 정지 확인, 원본 kubeconfig 보존, 정확한 RC에 연결한 schema 3 물리 증거.
- [ ] 호환성: CLI·JSON 변경 안내, 외부 Docker 연결 보존, v3 update manifest만
  게시하고 v0.1.2 이하의 재설치 경로 안내, 매니지드 K3s 프로필은 전환하지 않고 거부,
  Colima 상태 자동 변경 없음.
- [ ] 롤백: 업데이트 중단 시 이전 설치 유지, Docker 데이터 백업·복구 검토.
- [ ] 검증: 후보 소스에서 전체 `make test-local-macos` 통과, hosted 검증의 한계 명시,
  별도로 주장하는 물리 증거는 정확한 RC에 연결, 검증 후 재빌드 대체 없음.
- [ ] 배포: 보호된 배포 환경·hosted runner 확인, 저장소·workflow·소스·실행과 일치하는
  attestation, 정확한 해시, 불변 릴리스, 미해결 사항이 있으면 배포 중단.
