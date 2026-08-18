#!/bin/bash
# Verify the fail-closed GitHub state required before an automated 0.0.1 RC.
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
fetch "repos/$REPOSITORY/environments/hamn-validation/secrets" validation-secrets \
    "validation environment secret names"
fetch "repos/$REPOSITORY/environments/hamn-promotion/secrets" promotion-secrets \
    "promotion environment secret names"
fetch "repos/$REPOSITORY/rulesets" rulesets "repository rulesets"
fetch "repos/$REPOSITORY/immutable-releases" immutable-releases \
    "immutable release policy"
fetch "repos/$REPOSITORY/private-vulnerability-reporting" \
    private-vulnerability-reporting "private vulnerability reporting policy"
fetch "repos/$REPOSITORY/commits/main" main-commit "main commit verification"

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
require(isinstance(owner, dict) and isinstance(owner.get("id"), int),
        "repository owner identity is invalid")
security = repo.get("security_and_analysis")
require(isinstance(security, dict) and
        security.get("secret_scanning", {}).get("status") == "enabled" and
        security.get("secret_scanning_push_protection", {}).get("status") == "enabled",
        "secret scanning and push protection must be enabled")

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
validator = False
for runner in entries("runners", "runners"):
    if not isinstance(runner, dict):
        continue
    labels = runner.get("labels")
    label_names = {label.get("name") for label in labels if isinstance(label, dict)} \
        if isinstance(labels, list) else set()
    if runner.get("os") == "macOS" and runner.get("architecture") == "ARM64" and \
            runner.get("status") == "online" and \
            {"self-hosted", "hamn-validator"} <= label_names:
        validator = True
        break
require(validator, "an online macOS ARM64 hamn-validator runner is required")

variable_names = {item.get("name") for item in entries("variables", "variables")
                  if isinstance(item, dict)}
required_variables = {
    "HAMN_GUEST_IMAGE_URL",
    "HAMN_GUEST_IMAGE_SHA256",
    "HAMN_RELEASE_PUBLIC_KEY",
    "HAMN_VALIDATOR_IDENTITY",
    "HAMN_VALIDATOR_PUBLIC_KEY",
}
require(required_variables <= variable_names,
        "required release variables are missing: " +
        ", ".join(sorted(required_variables - variable_names)))
repository_secret_names = {item.get("name")
                           for item in entries("repository-secrets", "secrets")
                           if isinstance(item, dict)}
require(repository_secret_names == {"RELEASE_PLEASE_TOKEN"},
        "repository secrets must contain only RELEASE_PLEASE_TOKEN")

environment_names = {item.get("name") for item in entries("environments", "environments")
                     if isinstance(item, dict)}
require({"hamn-validation", "hamn-promotion"} <= environment_names,
        "locked release environments are missing")
validation_secret_names = {item.get("name")
                           for item in entries("validation-secrets", "secrets")
                           if isinstance(item, dict)}
promotion_secret_names = {item.get("name")
                          for item in entries("promotion-secrets", "secrets")
                          if isinstance(item, dict)}
require(validation_secret_names == {"HAMN_VALIDATOR_SIGNING_KEY"},
        "hamn-validation must contain only HAMN_VALIDATOR_SIGNING_KEY")
require(promotion_secret_names == {"HAMN_RELEASE_SIGNING_KEY"},
        "hamn-promotion must contain only HAMN_RELEASE_SIGNING_KEY")

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
commit = read("main-commit")
require(isinstance(commit, dict) and commit.get("commit", {}).get("verification", {}).get(
        "verified") is True, "main must resolve to a verified signed commit")
print("automated release repository preflight passed for " + repository)
PY
