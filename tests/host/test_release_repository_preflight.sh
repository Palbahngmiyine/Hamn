#!/bin/bash
# Repository preflight is a read-only safety check. The fixture models the
# exact public repository, release-only workflow, protected environments, and
# online physical validator required before an RC tag is created.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
WORK=$(mktemp -d /tmp/hamn-release-repository.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

BIN=$WORK/bin
mkdir "$BIN"
cat >"$BIN/gh" <<'EOF'
#!/bin/bash
set -euo pipefail
[ "${1:-}" = api ] || exit 64
case "${2:-}" in
repos/example/hamn)
    printf '%s\n' '{"full_name":"example/hamn","private":false,"visibility":"public","archived":false}'
    ;;
repos/example/hamn/actions/workflows)
    printf '%s\n' '{"workflows":[{"path":".github/workflows/release.yml","state":"active"}]}'
    ;;
repos/example/hamn/actions/permissions)
    if [ "${HAMN_TEST_WEAK_ACTIONS_POLICY:-0}" = 1 ]; then
        printf '%s\n' '{"enabled":true,"allowed_actions":"all","sha_pinning_required":false}'
    else
        printf '%s\n' '{"enabled":true,"allowed_actions":"selected","sha_pinning_required":true}'
    fi
    ;;
repos/example/hamn/actions/permissions/selected-actions)
    printf '%s\n' '{"github_owned_allowed":true,"verified_allowed":false,"patterns_allowed":[]}'
    ;;
repos/example/hamn/actions/permissions/workflow)
    printf '%s\n' '{"default_workflow_permissions":"read","can_approve_pull_request_reviews":false}'
    ;;
repos/example/hamn/environments)
    printf '%s\n' '{"environments":[{"name":"hamn-validation"},{"name":"hamn-promotion"}]}'
    ;;
repos/example/hamn/environments/hamn-validation)
    printf '%s\n' '{"name":"hamn-validation","protection_rules":[{"type":"required_reviewers"}]}'
    ;;
repos/example/hamn/environments/hamn-promotion)
    printf '%s\n' '{"name":"hamn-promotion","protection_rules":[{"type":"required_reviewers"}]}'
    ;;
repos/example/hamn/actions/runners)
    printf '%s\n' '{"runners":[{"os":"macOS","architecture":"ARM64","status":"online","labels":[{"name":"self-hosted"},{"name":"hamn-validator"}]}]}'
    ;;
repos/example/hamn/actions/variables)
    printf '%s\n' '{"variables":[{"name":"HAMN_GUEST_IMAGE_URL"},{"name":"HAMN_GUEST_IMAGE_SHA256"},{"name":"HAMN_VALIDATOR_IDENTITY"},{"name":"HAMN_RELEASE_PUBLIC_KEY"},{"name":"HAMN_VALIDATOR_PUBLIC_KEY"}]}'
    ;;
repos/example/hamn/environments/hamn-validation/secrets)
    if [ "${HAMN_TEST_MISSING_SECRET:-0}" = 1 ]; then
        printf '%s\n' '{"secrets":[]}'
    else
        printf '%s\n' '{"secrets":[{"name":"HAMN_VALIDATOR_SIGNING_KEY"}]}'
    fi
    ;;
repos/example/hamn/environments/hamn-promotion/secrets)
    if [ "${HAMN_TEST_MISSING_SECRET:-0}" = 1 ]; then
        printf '%s\n' '{"secrets":[]}'
    else
        printf '%s\n' '{"secrets":[{"name":"HAMN_RELEASE_SIGNING_KEY"}]}'
    fi
    ;;
*) exit 65 ;;
esac
EOF
chmod 0755 "$BIN/gh"

PATH="$BIN:$PATH" HAMN_RELEASE_REPOSITORY=example/hamn \
    "$ROOT/packaging/release/preflight-release-repository.sh" \
    >"$WORK/passed.out"
grep -Fxq 'release repository preflight passed for example/hamn' \
    "$WORK/passed.out"

if PATH="$BIN:$PATH" HAMN_RELEASE_REPOSITORY=example/hamn \
HAMN_TEST_MISSING_SECRET=1 \
    "$ROOT/packaging/release/preflight-release-repository.sh" \
    >"$WORK/missing.out" 2>"$WORK/missing.err"; then
    echo "FAIL: repository preflight accepted missing release secrets" >&2
    exit 1
fi
grep -Fq 'hamn-validation is missing HAMN_VALIDATOR_SIGNING_KEY' "$WORK/missing.err"

if HAMN_TEST_WEAK_ACTIONS_POLICY=1 \
    PATH="$BIN:$PATH" HAMN_RELEASE_REPOSITORY=example/hamn \
    "$ROOT/packaging/release/preflight-release-repository.sh" \
    >"$WORK/weak-policy.out" 2>"$WORK/weak-policy.err"; then
    echo "FAIL: repository preflight accepted an unrestricted Actions policy" >&2
    exit 1
fi
grep -Fq 'Actions must be enabled, limited to selected actions, and require SHA pins' \
    "$WORK/weak-policy.err"

echo "PASS: public release repository preflight is read-only and fail-closed"
