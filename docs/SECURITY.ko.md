# 보안 정책

Hamn은 Apple Silicon macOS에서 최신으로 공개된 stable release의 보안 제보를
받습니다. 공개되지 않은 source build와 local test fixture는 지원 배포 경로가
아닙니다.

## 취약점 제보

Repository의 **Security** 탭에 있는 private vulnerability-reporting form을
사용하세요. Public issue, pull request, log, discussion에 취약점을 공개하지
마세요.

Private reporting을 일시적으로 사용할 수 없다면, secure reporting channel을
요청하는 최소한의 public issue만 여세요. 그 issue에는 재현 상세, credential,
path, diagnostics archive, proof-of-concept code를 넣지 마세요.

## 제보에 포함할 내용

공개된 Hamn version, macOS version, Apple Silicon model, 최소 재현 절차, 기대 및
실제 동작, 예상 impact를 포함하세요. 첨부 파일에서는 Docker registry credential,
kubeconfig credential, SSH material, 개인 path를 제거하세요.

## Release 신뢰 경로

공개된 installer 또는 canonical manifest signature와 SHA-256을 검증한 release
artifact만으로 Hamn을 설치하세요. Installer와 `hamn update`는 서명되지 않았거나
호환되지 않는 host/guest artifact를 거부합니다.
