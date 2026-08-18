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
    printf '%s\n' '{"full_name":"example/hamn","private":false,"visibility":"public","archived":false,"owner":{"id":42,"login":"example","type":"User"},"security_and_analysis":{"secret_scanning":{"status":"enabled"},"secret_scanning_push_protection":{"status":"enabled"}}}'
    ;;
repos/example/hamn/collaborators?affiliation=all\&per_page=100)
    if [ "${HAMN_TEST_COLLABORATOR:-0}" = 1 ]; then
        printf '%s\n' '[{"login":"example","role_name":"admin","permissions":{"admin":true}},{"login":"outsider","role_name":"write","permissions":{"admin":false}}]'
    else
        printf '%s\n' '[{"login":"example","role_name":"admin","permissions":{"admin":true}}]'
    fi
    ;;
repos/example/hamn/invitations)
    if [ "${HAMN_TEST_INVITATION:-0}" = 1 ]; then
        printf '%s\n' '[{"id":7,"invitee":{"login":"pending-user"}}]'
    else
        printf '%s\n' '[]'
    fi
    ;;
repos/example/hamn/keys)
    if [ "${HAMN_TEST_DEPLOY_KEY:-0}" = 1 ]; then
        printf '%s\n' '[{"id":8,"title":"unexpected","read_only":false}]'
    else
        printf '%s\n' '[]'
    fi
    ;;
repos/example/hamn/actions/workflows)
    printf '%s\n' '{"workflows":[{"path":".github/workflows/release.yml","state":"active"},{"path":".github/workflows/ci.yml","state":"active"},{"path":".github/workflows/release-please.yml","state":"active"}]}'
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
        printf '%s\n' '{"github_owned_allowed":true,"verified_allowed":false,"patterns_allowed":["googleapis/release-please-action@*","cachix/install-nix-action@*"]}'
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
        printf '%s\n' '{"runners":[]}'
    fi
    ;;
repos/example/hamn/actions/variables)
    if [ "${HAMN_TEST_VARIABLE:-0}" = 1 ]; then
        printf '%s\n' '{"variables":[{"name":"UNTRUSTED_RELEASE_INPUT"}]}'
    else
        printf '%s\n' '{"variables":[]}'
    fi
    ;;
repos/example/hamn/actions/secrets)
    if [ "${HAMN_TEST_RELEASE_PLEASE_SECRET:-0}" = 1 ]; then
        printf '%s\n' '{"secrets":[]}'
    else
        printf '%s\n' '{"secrets":[{"name":"RELEASE_PLEASE_TOKEN"}]}'
    fi
    ;;
repos/example/hamn/environments)
    printf '%s\n' '{"environments":[{"name":"hamn-promotion"}]}'
    ;;
repos/example/hamn/environments/hamn-promotion)
    printf '%s\n' '{"id":7,"name":"hamn-promotion","can_admins_bypass":false,"protection_rules":[{"id":8,"type":"branch_policy"}],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}'
    ;;
repos/example/hamn/environments/hamn-promotion/secrets)
    if [ "${HAMN_TEST_SECRET:-0}" = 1 ]; then
        printf '%s\n' '{"secrets":[{"name":"HAMN_RELEASE_SIGNING_KEY"}]}'
    else
        printf '%s\n' '{"secrets":[]}'
    fi
    ;;
repos/example/hamn/environments/hamn-promotion/variables)
    printf '%s\n' '{"variables":[]}'
    ;;
repos/example/hamn/environments/hamn-promotion/deployment-branch-policies)
    if [ "${HAMN_TEST_BRANCH_POLICY:-0}" = 1 ]; then
        printf '%s\n' '{"branch_policies":[{"name":"release/*","type":"branch"}]}'
    else
        printf '%s\n' '{"branch_policies":[{"name":"main","type":"branch"}]}'
    fi
    ;;
repos/example/hamn/rulesets)
    printf '%s\n' '[{"id":1,"name":"protect-main-and-release-workflow","enforcement":"active"},{"id":2,"name":"immutable-stable-releases","enforcement":"active"}]'
    ;;
repos/example/hamn/rulesets/1)
    if [ "${HAMN_TEST_WEAK_RULESET:-0}" = 1 ]; then
        printf '%s\n' '{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"},{"type":"required_linear_history"},{"type":"pull_request","parameters":{"required_approving_review_count":1,"dismiss_stale_reviews_on_push":true,"required_reviewers":[],"require_code_owner_review":false,"require_last_push_approval":true,"required_review_thread_resolution":true,"allowed_merge_methods":["squash","rebase"]}},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":false,"required_status_checks":[]}}],"bypass_actors":[]}'
    else
        printf '%s\n' '{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"},{"type":"required_linear_history"},{"type":"pull_request","parameters":{"required_approving_review_count":0,"dismiss_stale_reviews_on_push":false,"required_reviewers":[],"require_code_owner_review":false,"require_last_push_approval":false,"required_review_thread_resolution":true,"allowed_merge_methods":["squash","rebase"]}},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":true,"required_status_checks":[{"context":"Portable source gates"},{"context":"macOS build and regression gates"}]}}],"bypass_actors":[]}'
    fi
    ;;
repos/example/hamn/rulesets/2)
    printf '%s\n' '{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v*"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}'
    ;;
repos/example/hamn/immutable-releases)
    printf '%s\n' '{"enabled":true,"enforced_by_owner":false}'
    ;;
repos/example/hamn/private-vulnerability-reporting)
    printf '%s\n' '{"enabled":true}'
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
    'only GitHub-owned Actions, Nix, and Release Please may run' action
assert_failure HAMN_TEST_RELEASE_PLEASE_SECRET \
    'repository secrets must contain only RELEASE_PLEASE_TOKEN' release-please-secret
assert_failure HAMN_TEST_RUNNER \
    'keyless hosted releases must not use repository self-hosted runners' runner
assert_failure HAMN_TEST_VARIABLE \
    'keyless hosted releases must not depend on repository variables' variable
assert_failure HAMN_TEST_SECRET \
    'hamn-promotion must not contain secrets or variables' secret
assert_failure HAMN_TEST_BRANCH_POLICY \
    'hamn-promotion must allow only the main branch' branch-policy
assert_failure HAMN_TEST_WEAK_RULESET \
    'main pull request rules are not solo-maintainer safe' ruleset
assert_failure HAMN_TEST_COLLABORATOR \
    'repository must have exactly one collaborator: its owner' collaborator
assert_failure HAMN_TEST_INVITATION \
    'repository must not have pending invitations' invitation
assert_failure HAMN_TEST_DEPLOY_KEY \
    'repository must not have deploy keys' deploy-key

echo "PASS: automated release repository preflight is read-only and fail-closed"
