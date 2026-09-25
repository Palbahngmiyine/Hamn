#!/bin/bash
# Verify the fail-closed GitHub state required before a keyless hosted release.
# This check is read-only and verifies only secret names, never secret values.
set -euo pipefail
export LC_ALL=C

fail() {
    echo "hamn release repository preflight: $*" >&2
    exit 1
}

REPOSITORY=${HAMN_RELEASE_REPOSITORY:-}
if [ -z "$REPOSITORY" ]; then
    command -v gh >/dev/null 2>&1 || fail "GitHub CLI (gh) is required"
    REPOSITORY=$(gh repo view --json nameWithOwner --jq .nameWithOwner) ||
        fail "cannot resolve the current GitHub repository"
fi
[[ "$REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
    fail "HAMN_RELEASE_REPOSITORY must be owner/repository"
command -v gh >/dev/null 2>&1 || fail "GitHub CLI (gh) is required"
[ -x "${HAMN_DEV:-}" ] || fail "HAMN_DEV must name the built hamn-dev executable"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/hamn-release-repository.XXXXXX") ||
    fail "cannot create preflight workspace"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

fetch() {
    local endpoint=$1
    local output=$2
    local description=$3
    gh api "$endpoint" >"$WORK/$output.json" || fail "cannot read $description"
}

fetch "repos/$REPOSITORY" repository "repository metadata"
fetch "repos/$REPOSITORY/collaborators?affiliation=all&per_page=100" collaborators \
    "repository collaborators"
fetch "repos/$REPOSITORY/invitations" invitations "pending repository invitations"
fetch "repos/$REPOSITORY/keys" deploy-keys "repository deploy keys"
fetch "repos/$REPOSITORY/actions/workflows" workflows "repository workflows"
fetch "repos/$REPOSITORY/actions/permissions" actions-permissions "Actions permissions"
fetch "repos/$REPOSITORY/actions/permissions/selected-actions" selected-actions \
    "allowed Actions policy"
fetch "repos/$REPOSITORY/actions/permissions/workflow" workflow-permissions \
    "default workflow token permissions"
fetch "repos/$REPOSITORY/actions/permissions/fork-pr-contributor-approval" \
    fork-approval "fork workflow approval policy"
fetch "repos/$REPOSITORY/actions/runners" runners "repository runners"
fetch "repos/$REPOSITORY/actions/variables" variables "repository variables"
fetch "repos/$REPOSITORY/actions/secrets" repository-secrets "repository secrets"
fetch "repos/$REPOSITORY/environments" environments "repository environments"
fetch "repos/$REPOSITORY/environments/hamn-promotion" promotion \
    "promotion environment"
fetch "repos/$REPOSITORY/environments/hamn-promotion/secrets" promotion-secrets \
    "promotion environment secret names"
fetch "repos/$REPOSITORY/environments/hamn-promotion/variables" promotion-variables \
    "promotion environment variables"
fetch "repos/$REPOSITORY/environments/hamn-promotion/deployment-branch-policies" \
    promotion-branches "promotion branch policies"
fetch "repos/$REPOSITORY/rulesets" rulesets "repository rulesets"
fetch "repos/$REPOSITORY/immutable-releases" immutable-releases \
    "immutable release policy"
fetch "repos/$REPOSITORY/private-vulnerability-reporting" \
    private-vulnerability-reporting "private vulnerability reporting policy"

"$HAMN_DEV" release preflight-rulesets "$WORK/rulesets.json" "$WORK/ruleset-ids"

while IFS=$'\t' read -r label ruleset_id; do
    [[ "$label" =~ ^[a-z-]+$ ]] && [[ "$ruleset_id" =~ ^[1-9][0-9]*$ ]] ||
        fail "repository ruleset identity is invalid"
    fetch "repos/$REPOSITORY/rulesets/$ruleset_id" "ruleset-$label" \
        "$label ruleset"
done <"$WORK/ruleset-ids"

"$HAMN_DEV" release preflight-repository "$REPOSITORY" "$WORK"
