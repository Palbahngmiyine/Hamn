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
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

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

python3 - "$WORK/rulesets.json" "$WORK/ruleset-ids" <<'PY'
import json
import sys

source_path, output_path = sys.argv[1:]
expected = {
    "protect-main-and-release-workflow": "main",
    "immutable-stable-releases": "stable-immutable",
}
with open(source_path, encoding="utf-8") as source:
    rulesets = json.load(source)
if not isinstance(rulesets, list):
    raise SystemExit("repository ruleset response is invalid")
found = {}
for ruleset in rulesets:
    if not isinstance(ruleset, dict) or not isinstance(ruleset.get("name"), str):
        raise SystemExit("repository ruleset entry is invalid")
    name = ruleset["name"]
    if name in found:
        raise SystemExit("repository contains duplicate release rulesets")
    found[name] = ruleset
if set(found) != set(expected):
    raise SystemExit("repository release ruleset set is invalid")
with open(output_path, "w", encoding="utf-8", newline="\n") as output:
    for name, label in expected.items():
        ruleset = found[name]
        if ruleset.get("enforcement") != "active" or \
                not isinstance(ruleset.get("id"), int):
            raise SystemExit(name + " must be active")
        output.write(label + "\t" + str(ruleset["id"]) + "\n")
PY

while IFS=$'\t' read -r label ruleset_id; do
    [[ "$label" =~ ^[a-z-]+$ ]] && [[ "$ruleset_id" =~ ^[1-9][0-9]*$ ]] ||
        fail "repository ruleset identity is invalid"
    fetch "repos/$REPOSITORY/rulesets/$ruleset_id" "ruleset-$label" \
        "$label ruleset"
done <"$WORK/ruleset-ids"

python3 - "$REPOSITORY" "$WORK" <<'PY'
import json
import os
import sys

repository, directory = sys.argv[1:]

def read(name):
    with open(os.path.join(directory, name + ".json"), encoding="utf-8") as source:
        return json.load(source)

def require(condition, message):
    if not condition:
        raise SystemExit(message)

def entries(name, key):
    value = read(name)
    items = value.get(key) if isinstance(value, dict) else None
    require(isinstance(items, list), name + " response is invalid")
    return items

def check_ruleset(name, target, includes, rule_types, bypass_actors):
    value = read("ruleset-" + name)
    require(isinstance(value, dict) and value.get("target") == target and
            value.get("enforcement") == "active", name + " ruleset is invalid")
    conditions = value.get("conditions")
    refs = conditions.get("ref_name") if isinstance(conditions, dict) else None
    require(isinstance(refs, dict) and refs.get("include") == includes and
            refs.get("exclude") == [], name + " ruleset target is invalid")
    rules = value.get("rules")
    require(isinstance(rules, list) and
            {rule.get("type") for rule in rules if isinstance(rule, dict)} == rule_types,
            name + " ruleset rules are invalid")
    require(value.get("bypass_actors") == bypass_actors,
            name + " ruleset bypass actors are invalid")
    return rules

repo = read("repository")
require(isinstance(repo, dict) and repo.get("full_name") == repository,
        "repository identity is invalid")
require(repo.get("private") is False and repo.get("visibility") == "public" and
        repo.get("archived") is False, "repository must be active and public")
owner = repo.get("owner")
repository_owner = repository.split("/", 1)[0]
require(isinstance(owner, dict) and isinstance(owner.get("id"), int) and
        owner.get("login") == repository_owner and owner.get("type") == "User",
        "repository owner identity is invalid")
security = repo.get("security_and_analysis")
require(isinstance(security, dict) and
        security.get("secret_scanning", {}).get("status") == "enabled" and
        security.get("secret_scanning_push_protection", {}).get("status") == "enabled",
        "secret scanning and push protection must be enabled")

collaborators = read("collaborators")
require(isinstance(collaborators, list) and len(collaborators) == 1,
        "repository must have exactly one collaborator: its owner")
collaborator = collaborators[0]
permissions = collaborator.get("permissions") if isinstance(collaborator, dict) else None
require(isinstance(collaborator, dict) and collaborator.get("login") == repository_owner and
        collaborator.get("role_name") == "admin" and isinstance(permissions, dict) and
        permissions.get("admin") is True,
        "repository's only collaborator must be its owner with admin access")
require(read("invitations") == [], "repository must not have pending invitations")
require(read("deploy-keys") == [], "repository must not have deploy keys")

workflows = entries("workflows", "workflows")
observed_workflows = sorted((item.get("path"), item.get("state"))
                            for item in workflows if isinstance(item, dict))
require(observed_workflows == [
            (".github/workflows/ci.yml", "active"),
            (".github/workflows/release-please.yml", "active"),
            (".github/workflows/release.yml", "active"),
        ], "only CI, Release Please, and release workflows may be active")
actions = read("actions-permissions")
require(isinstance(actions, dict) and actions.get("enabled") is True and
        actions.get("allowed_actions") == "selected" and
        actions.get("sha_pinning_required") is True,
        "Actions must be enabled, selected, and SHA-pinned")
selected = read("selected-actions")
require(isinstance(selected, dict) and
        selected.get("github_owned_allowed") is True and
        selected.get("verified_allowed") is False and
        set(selected.get("patterns_allowed", [])) == {
            "cachix/install-nix-action@*",
            "googleapis/release-please-action@*",
        }, "only GitHub-owned Actions, Nix, and Release Please may run")
workflow = read("workflow-permissions")
require(isinstance(workflow, dict) and
        workflow.get("default_workflow_permissions") == "read" and
        workflow.get("can_approve_pull_request_reviews") is False,
        "default GITHUB_TOKEN permissions must be read-only")
require(read("fork-approval") == {"approval_policy": "all_external_contributors"},
        "all external fork workflows must require approval")
require(entries("runners", "runners") == [],
        "keyless hosted releases must not use repository self-hosted runners")

variable_names = {item.get("name") for item in entries("variables", "variables")
                  if isinstance(item, dict)}
require(variable_names == set(),
        "keyless hosted releases must not depend on repository variables")
repository_secret_names = {item.get("name")
                           for item in entries("repository-secrets", "secrets")
                           if isinstance(item, dict)}
require(repository_secret_names == {"RELEASE_PLEASE_TOKEN"},
        "repository secrets must contain only RELEASE_PLEASE_TOKEN")

environment_names = {item.get("name") for item in entries("environments", "environments")
                     if isinstance(item, dict)}
require(environment_names == {"hamn-promotion"},
        "hamn-promotion must be the only release environment")
promotion = read("promotion")
require(isinstance(promotion, dict) and promotion.get("name") == "hamn-promotion" and
        promotion.get("can_admins_bypass") is False and
        promotion.get("deployment_branch_policy") == {
            "protected_branches": False,
            "custom_branch_policies": True,
        }, "hamn-promotion must be fail-closed and use custom branch policies")
protection_rules = promotion.get("protection_rules")
require(isinstance(protection_rules, list) and len(protection_rules) == 1 and
        protection_rules[0].get("type") == "branch_policy",
        "hamn-promotion must enforce its branch policy")
promotion_branches = entries("promotion-branches", "branch_policies")
require(len(promotion_branches) == 1 and
        promotion_branches[0].get("name") == "main" and
        promotion_branches[0].get("type") == "branch",
        "hamn-promotion must allow only the main branch")
promotion_secret_names = {item.get("name")
                          for item in entries("promotion-secrets", "secrets")
                          if isinstance(item, dict)}
promotion_variable_names = {item.get("name")
                            for item in entries("promotion-variables", "variables")
                            if isinstance(item, dict)}
require(promotion_secret_names == set() and promotion_variable_names == set(),
        "hamn-promotion must not contain secrets or variables")

main_rules = check_ruleset("main", "branch", ["~DEFAULT_BRANCH"],
                           {"deletion", "non_fast_forward", "required_linear_history",
                            "pull_request", "required_status_checks"}, [])
pull = next(rule for rule in main_rules if rule.get("type") == "pull_request")
parameters = pull.get("parameters")
require(isinstance(parameters, dict) and parameters == {
            "required_approving_review_count": 0,
            "dismiss_stale_reviews_on_push": False,
            "required_reviewers": [],
            "require_code_owner_review": False,
            "require_last_push_approval": False,
            "required_review_thread_resolution": True,
            "allowed_merge_methods": ["squash", "rebase"],
        }, "main pull request rules are not solo-maintainer safe")
status = next(rule for rule in main_rules if rule.get("type") ==
              "required_status_checks")
parameters = status.get("parameters")
checks = parameters.get("required_status_checks") if isinstance(parameters, dict) else None
contexts = {check.get("context") for check in checks if isinstance(check, dict)} \
    if isinstance(checks, list) else set()
require(isinstance(parameters, dict) and
        parameters.get("strict_required_status_checks_policy") is True and
        contexts == {"Portable source gates", "macOS build and regression gates"},
        "main must require both Nix CI status checks on the latest commit")

check_ruleset("stable-immutable", "tag", ["refs/tags/v*"],
              {"deletion", "non_fast_forward"}, [])

require(read("immutable-releases").get("enabled") is True,
        "immutable releases must be enabled")
require(read("private-vulnerability-reporting") == {"enabled": True},
        "private vulnerability reporting must be enabled")
print("automated release repository preflight passed for " + repository)
PY
