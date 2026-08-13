#!/bin/bash
# Validate the GitHub configuration required before a Hamn 0.0.1 RC tag can
# create and physically validate a candidate. This is read-only: it does not
# create repositories, environments, runners, variables, secrets, tags, or
# releases.
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

gh api "repos/$REPOSITORY" >"$WORK/repository.json" ||
    fail "cannot read repository metadata"
gh api "repos/$REPOSITORY/actions/workflows" >"$WORK/workflows.json" ||
    fail "cannot read repository workflows"
gh api "repos/$REPOSITORY/actions/permissions" >"$WORK/actions-permissions.json" ||
    fail "cannot read repository Actions permissions"
gh api "repos/$REPOSITORY/actions/permissions/selected-actions" \
    >"$WORK/selected-actions.json" ||
    fail "cannot read the repository allowed Actions policy"
gh api "repos/$REPOSITORY/actions/permissions/workflow" \
    >"$WORK/workflow-permissions.json" ||
    fail "cannot read the default workflow token permissions"
gh api "repos/$REPOSITORY/environments" >"$WORK/environments.json" ||
    fail "cannot read repository environments"
gh api "repos/$REPOSITORY/environments/hamn-validation" \
    >"$WORK/validation-environment.json" ||
    fail "cannot read the protected validation environment"
gh api "repos/$REPOSITORY/environments/hamn-promotion" \
    >"$WORK/promotion-environment.json" ||
    fail "cannot read the protected promotion environment"
gh api "repos/$REPOSITORY/actions/runners" >"$WORK/runners.json" ||
    fail "cannot read repository runners"
gh api "repos/$REPOSITORY/actions/variables" >"$WORK/variables.json" ||
    fail "cannot read repository variables"
gh api "repos/$REPOSITORY/environments/hamn-validation/secrets" \
    >"$WORK/validation-secrets.json" ||
    fail "cannot read validation environment secret names"
gh api "repos/$REPOSITORY/environments/hamn-promotion/secrets" \
    >"$WORK/promotion-secrets.json" ||
    fail "cannot read promotion environment secret names"

python3 - "$REPOSITORY" "$WORK" <<'PY'
import json
import os
import re
import sys

repository, directory = sys.argv[1:]

def read(name):
    path = os.path.join(directory, name + ".json")
    with open(path, encoding="utf-8") as source:
        return json.load(source)

def require(condition, message):
    if not condition:
        raise SystemExit(message)

repo = read("repository")
require(isinstance(repo, dict) and repo.get("full_name") == repository,
        "repository identity is invalid")
require(repo.get("private") is False and repo.get("visibility") == "public",
        "repository must be public before release")
require(repo.get("archived") is False,
        "repository must not be archived")

workflows = read("workflows")
items = workflows.get("workflows") if isinstance(workflows, dict) else None
require(isinstance(items, list), "workflow response is invalid")
paths = {}
for workflow in items:
    require(isinstance(workflow, dict) and isinstance(workflow.get("path"), str) and
            isinstance(workflow.get("state"), str), "workflow entry is invalid")
    paths[workflow["path"]] = workflow["state"]
require(paths == {".github/workflows/release.yml": "active"},
        "release repository must expose only an active release workflow")

actions_permissions = read("actions-permissions")
require(isinstance(actions_permissions, dict) and
        actions_permissions.get("enabled") is True and
        actions_permissions.get("allowed_actions") == "selected" and
        actions_permissions.get("sha_pinning_required") is True,
        "Actions must be enabled, limited to selected actions, and require SHA pins")
selected_actions = read("selected-actions")
require(isinstance(selected_actions, dict) and
        selected_actions.get("github_owned_allowed") is True and
        selected_actions.get("verified_allowed") is False and
        selected_actions.get("patterns_allowed") == [],
        "only GitHub-owned actions may be allowed")
workflow_permissions = read("workflow-permissions")
require(isinstance(workflow_permissions, dict) and
        workflow_permissions.get("default_workflow_permissions") == "read" and
        workflow_permissions.get("can_approve_pull_request_reviews") is False,
        "default GITHUB_TOKEN permissions must be read-only")

environments = read("environments")
items = environments.get("environments") if isinstance(environments, dict) else None
require(isinstance(items, list), "environment response is invalid")
names = {item.get("name") for item in items if isinstance(item, dict)}
require({"hamn-validation", "hamn-promotion"} <= names,
        "hamn-validation and hamn-promotion environments are required")
for name in ("hamn-validation", "hamn-promotion"):
    environment = read(name.split("-", 1)[1] + "-environment")
    require(isinstance(environment, dict) and environment.get("name") == name,
            name + " environment identity is invalid")
    protection_rules = environment.get("protection_rules")
    require(isinstance(protection_rules, list) and any(
            isinstance(rule, dict) and rule.get("type") == "required_reviewers"
            for rule in protection_rules),
            name + " must require manual reviewer approval")

runners = read("runners")
items = runners.get("runners") if isinstance(runners, dict) else None
require(isinstance(items, list), "runner response is invalid")
validator = False
for runner in items:
    if not isinstance(runner, dict):
        continue
    labels = runner.get("labels")
    label_names = {label.get("name") for label in labels if isinstance(label, dict)} \
        if isinstance(labels, list) else set()
    if (runner.get("os") == "macOS" and runner.get("architecture") == "ARM64" and
            runner.get("status") == "online" and "self-hosted" in label_names and
            "hamn-validator" in label_names):
        validator = True
        break
require(validator, "an online macOS ARM64 hamn-validator runner is required")

variables = read("variables")
items = variables.get("variables") if isinstance(variables, dict) else None
require(isinstance(items, list), "variable response is invalid")
names = {item.get("name") for item in items if isinstance(item, dict)}
required_variables = {
    "HAMN_GUEST_IMAGE_URL",
    "HAMN_GUEST_IMAGE_SHA256",
    "HAMN_VALIDATOR_IDENTITY",
    "HAMN_RELEASE_PUBLIC_KEY",
    "HAMN_VALIDATOR_PUBLIC_KEY",
}
require(required_variables <= names,
        "required release variables are missing: " +
        ", ".join(sorted(required_variables - names)))

def secret_names(label):
    value = read(label + "-secrets")
    items = value.get("secrets") if isinstance(value, dict) else None
    require(isinstance(items, list), label + " environment secret response is invalid")
    return {item.get("name") for item in items if isinstance(item, dict)}

require({"HAMN_VALIDATOR_SIGNING_KEY"} <= secret_names("validation"),
        "hamn-validation is missing HAMN_VALIDATOR_SIGNING_KEY")
require({"HAMN_RELEASE_SIGNING_KEY"} <= secret_names("promotion"),
        "hamn-promotion is missing HAMN_RELEASE_SIGNING_KEY")

print("release repository preflight passed for " + repository)
PY
