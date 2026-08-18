#!/bin/bash
# Repository preflight permits only the pinned CI and release automation paths.
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
    printf '%s\n' '{"full_name":"example/hamn","private":false,"visibility":"public","archived":false,"owner":{"id":42},"security_and_analysis":{"secret_scanning":{"status":"enabled"},"secret_scanning_push_protection":{"status":"enabled"}}}'
    ;;
repos/example/hamn/actions/workflows)
    printf '%s\n' '{"workflows":[{"path":".github/workflows/release.yml","state":"active"},{"path":".github/workflows/ci.yml","state":"active"}]}'
    ;;
repos/example/hamn/actions/permissions)
    if [ "${HAMN_TEST_WEAK_ACTIONS_POLICY:-0}" = 1 ]; then
        printf '%s\n' '{"enabled":true,"allowed_actions":"all"}'
    else
        printf '%s\n' '{"enabled":true,"allowed_actions":"selected","sha_pinning_required":true}'
    fi
    ;;
repos/example/hamn/actions/permissions/selected-actions)
    if [ "${HAMN_TEST_UNSAFE_ACTION:-0}" = 1 ]; then
        printf '%s\n' '{"github_owned_allowed":true,"verified_allowed":true,"patterns_allowed":[]}'
    else
        printf '%s\n' '{"github_owned_allowed":true,"verified_allowed":false,"patterns_allowed":["cachix/install-nix-action@*"]}'
    fi
    ;;
repos/example/hamn/actions/permissions/workflow)
    printf '%s\n' '{"default_workflow_permissions":"read","can_approve_pull_request_reviews":false}'
    ;;
repos/example/hamn/actions/permissions/fork-pr-contributor-approval)
    printf '%s\n' '{"approval_policy":"all_external_contributors"}'
    ;;
repos/example/hamn/actions/runners)
    if [ "${HAMN_TEST_RUNNER:-0}" = 1 ]; then
        printf '%s\n' '{"runners":[{"name":"unsafe-runner"}]}'
    else
        printf '%s\n' '{"runners":[{"name":"hamn-validator","os":"macOS","architecture":"ARM64","status":"online","labels":[{"name":"self-hosted"},{"name":"macOS"},{"name":"ARM64"},{"name":"hamn-validator"}]}]}'
    fi
    ;;
repos/example/hamn/actions/variables)
    printf '%s\n' '{"variables":[{"name":"HAMN_GUEST_IMAGE_URL"},{"name":"HAMN_GUEST_IMAGE_SHA256"},{"name":"HAMN_RELEASE_PUBLIC_KEY"},{"name":"HAMN_VALIDATOR_IDENTITY"},{"name":"HAMN_VALIDATOR_PUBLIC_KEY"}]}'
    ;;
repos/example/hamn/actions/secrets)
    printf '%s\n' '{"secrets":[]}'
    ;;
repos/example/hamn/environments)
    printf '%s\n' '{"environments":[{"name":"hamn-validation"},{"name":"hamn-promotion"}]}'
    ;;
repos/example/hamn/environments/hamn-validation/secrets)
    if [ "${HAMN_TEST_SECRET:-0}" = 1 ]; then
        printf '%s\n' '{"secrets":[]}'
    else
        printf '%s\n' '{"secrets":[{"name":"HAMN_VALIDATOR_SIGNING_KEY"}]}'
    fi
    ;;
repos/example/hamn/environments/hamn-promotion/secrets)
    printf '%s\n' '{"secrets":[{"name":"HAMN_RELEASE_SIGNING_KEY"}]}'
    ;;
repos/example/hamn/rulesets)
    printf '%s\n' '[{"id":1,"name":"protect-main-and-release-workflow","enforcement":"active"},{"id":2,"name":"immutable-v0.0.1-release-candidates","enforcement":"active"},{"id":3,"name":"immutable-stable-v0.0.1","enforcement":"active"},{"id":4,"name":"v0.0.1-release-candidates-owner-created-only","enforcement":"active"},{"id":5,"name":"stable-v0.0.1-owner-created-only","enforcement":"active"}]'
    ;;
repos/example/hamn/rulesets/1)
    if [ "${HAMN_TEST_WEAK_RULESET:-0}" = 1 ]; then
        printf '%s\n' '{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"},{"type":"required_linear_history"},{"type":"pull_request","parameters":{"required_approving_review_count":1,"dismiss_stale_reviews_on_push":true,"required_reviewers":[],"require_code_owner_review":false,"require_last_push_approval":true,"required_review_thread_resolution":true,"allowed_merge_methods":["squash","rebase"]}},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":false,"required_status_checks":[]}}],"bypass_actors":[]}'
    else
        printf '%s\n' '{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"},{"type":"required_linear_history"},{"type":"pull_request","parameters":{"required_approving_review_count":0,"dismiss_stale_reviews_on_push":false,"required_reviewers":[],"require_code_owner_review":false,"require_last_push_approval":false,"required_review_thread_resolution":true,"allowed_merge_methods":["squash","rebase"]}},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":true,"required_status_checks":[{"context":"Portable source gates"},{"context":"macOS build and regression gates"}]}}],"bypass_actors":[]}'
    fi
    ;;
repos/example/hamn/rulesets/2)
    printf '%s\n' '{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v0.0.1-rc.*"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}'
    ;;
repos/example/hamn/rulesets/3)
    printf '%s\n' '{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v0.0.1"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}'
    ;;
repos/example/hamn/rulesets/4)
    printf '%s\n' '{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v0.0.1-rc.*"],"exclude":[]}},"rules":[{"type":"creation"}],"bypass_actors":[{"actor_id":42,"actor_type":"User","bypass_mode":"always"}]}'
    ;;
repos/example/hamn/rulesets/5)
    printf '%s\n' '{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v0.0.1"],"exclude":[]}},"rules":[{"type":"creation"}],"bypass_actors":[{"actor_id":42,"actor_type":"User","bypass_mode":"always"}]}'
    ;;
repos/example/hamn/immutable-releases)
    printf '%s\n' '{"enabled":true,"enforced_by_owner":false}'
    ;;
repos/example/hamn/private-vulnerability-reporting)
    printf '%s\n' '{"enabled":true}'
    ;;
repos/example/hamn/commits/main)
    if [ "${HAMN_TEST_UNSIGNED:-0}" = 1 ]; then
        printf '%s\n' '{"commit":{"verification":{"verified":false}}}'
    else
        printf '%s\n' '{"commit":{"verification":{"verified":true}}}'
    fi
    ;;
repos/example/hamn/tags|repos/example/hamn/releases)
    printf '%s\n' '[]'
    ;;
*) exit 65 ;;
esac
EOF
chmod 0755 "$BIN/gh"

run_preflight() {
    PATH="$BIN:$PATH" HAMN_RELEASE_REPOSITORY=example/hamn \
        "$ROOT/packaging/release/preflight-release-repository.sh"
}

run_preflight >"$WORK/passed.out"
grep -Fxq 'automated release repository preflight passed for example/hamn' \
    "$WORK/passed.out"

assert_failure() {
    local variable=$1
    local message=$2
    local label=$3
    if env "$variable=1" PATH="$BIN:$PATH" HAMN_RELEASE_REPOSITORY=example/hamn \
        "$ROOT/packaging/release/preflight-release-repository.sh" \
        >"$WORK/$label.out" 2>"$WORK/$label.err"; then
        echo "FAIL: repository preflight accepted $label" >&2
        exit 1
    fi
    grep -Fq "$message" "$WORK/$label.err"
}

assert_failure HAMN_TEST_WEAK_ACTIONS_POLICY \
    'Actions must be enabled, selected, and SHA-pinned' actions
assert_failure HAMN_TEST_UNSAFE_ACTION \
    'only GitHub-owned Actions and the pinned Nix installer may run' action
assert_failure HAMN_TEST_RUNNER \
    'an online macOS ARM64 hamn-validator runner is required' runner
assert_failure HAMN_TEST_SECRET \
    'hamn-validation must contain only HAMN_VALIDATOR_SIGNING_KEY' secret
assert_failure HAMN_TEST_WEAK_RULESET \
    'main pull request rules are not solo-maintainer safe' ruleset
assert_failure HAMN_TEST_UNSIGNED \
    'main must resolve to a verified signed commit' unsigned

echo "PASS: automated release repository preflight is read-only and fail-closed"
