//! `preflight-repository`: read-only checks of the GitHub repository state
//! a keyless hosted release depends on.
//!
//! `HAMN_RELEASE_REPOSITORY` names the repository (`owner/repository`;
//! default: the current directory's, from `gh repo view`). Every response
//! comes from a `gh api` GET of [`ENDPOINTS`] and of each release ruleset,
//! each bounded by [`GH_TIMEOUT`]; nothing on GitHub is changed and only
//! secret names, never values, are read. The checks then judge the
//! responses; the first unmet requirement is the error.
use super::checkout::{environment, variable};
use super::process::{self, Spec};
use super::runtime::which;
use super::syntax::is_repository;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

/// Release rulesets by name, with the label under which their details are
/// fetched.
const RULESETS: [(&str, &str); 2] =
    [("protect-main-and-release-workflow", "main"), ("immutable-stable-releases", "stable-immutable")];

/// Responses by name: the API path below `repos/OWNER/REPOSITORY` and what
/// a failure to read it is called.
const ENDPOINTS: [(&str, &str, &str); 20] = [
    ("repository", "", "repository metadata"),
    ("collaborators", "/collaborators?affiliation=all&per_page=100", "repository collaborators"),
    ("invitations", "/invitations", "pending repository invitations"),
    ("deploy-keys", "/keys", "repository deploy keys"),
    ("workflows", "/actions/workflows", "repository workflows"),
    ("actions-permissions", "/actions/permissions", "Actions permissions"),
    ("selected-actions", "/actions/permissions/selected-actions", "allowed Actions policy"),
    ("workflow-permissions", "/actions/permissions/workflow", "default workflow token permissions"),
    ("fork-approval", "/actions/permissions/fork-pr-contributor-approval", "fork workflow approval policy"),
    ("runners", "/actions/runners", "repository runners"),
    ("variables", "/actions/variables", "repository variables"),
    ("repository-secrets", "/actions/secrets", "repository secrets"),
    ("environments", "/environments", "repository environments"),
    ("promotion", "/environments/hamn-promotion", "promotion environment"),
    ("promotion-secrets", "/environments/hamn-promotion/secrets", "promotion environment secret names"),
    ("promotion-variables", "/environments/hamn-promotion/variables", "promotion environment variables"),
    ("promotion-branches", "/environments/hamn-promotion/deployment-branch-policies", "promotion branch policies"),
    ("rulesets", "/rulesets", "repository rulesets"),
    ("immutable-releases", "/immutable-releases", "immutable release policy"),
    ("private-vulnerability-reporting", "/private-vulnerability-reporting", "private vulnerability reporting policy"),
];

/// Each `gh` call; the API answers well within it.
pub const GH_TIMEOUT: Duration = Duration::from_secs(60);

/// `preflight-repository`; see the module documentation.
pub fn repository(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("usage: hamn-dev release preflight-repository (HAMN_RELEASE_REPOSITORY=OWNER/REPOSITORY)".into());
    }
    let repository = preflight().map_err(|error| format!("release repository preflight: {error}"))?;
    println!("automated release repository preflight passed for {repository}");
    Ok(())
}

fn preflight() -> Result<String, String> {
    let gh = which("gh", std::env::var_os("PATH").as_deref());
    let environment = environment(&[])?;
    let spec = Spec { environment: Some(&environment), ..Spec::default() };
    let gh_output = |gh: &Path, args: &[&str]| process::run(gh.as_os_str(), args, &spec, GH_TIMEOUT);
    let mut repository = variable("HAMN_RELEASE_REPOSITORY")?;
    if repository.is_empty() {
        let gh = gh.as_deref().ok_or("GitHub CLI (gh) is required")?;
        let current = gh_output(gh, &["repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"])
            .map_err(|error| format!("cannot resolve the current GitHub repository: {error}"))?;
        repository = current.trim_end_matches('\n').to_owned();
    }
    if !is_repository(&repository) {
        return Err("HAMN_RELEASE_REPOSITORY must be owner/repository".into());
    }
    let gh = gh.ok_or("GitHub CLI (gh) is required")?;
    let fetch = |endpoint: &str, description: &str| -> Result<Value, String> {
        let text = gh_output(&gh, &["api", endpoint]).map_err(|error| format!("cannot read {description}: {error}"))?;
        serde_json::from_str(&text).map_err(|error| format!("{description} response is not valid JSON: {error}"))
    };
    let mut responses = BTreeMap::new();
    for (name, path, description) in ENDPOINTS {
        responses.insert(name.to_owned(), fetch(&format!("repos/{repository}{path}"), description)?);
    }
    for (label, id) in ruleset_ids(&responses["rulesets"])? {
        let description = format!("{label} ruleset");
        responses
            .insert(format!("ruleset-{label}"), fetch(&format!("repos/{repository}/rulesets/{id}"), &description)?);
    }
    check(&repository, &Responses { responses })?;
    Ok(repository)
}

/// The label and positive ID of each release ruleset: the listing must hold
/// exactly the release rulesets, each active.
fn ruleset_ids(listed: &Value) -> Result<Vec<(&'static str, u64)>, String> {
    let listed = listed.as_array().ok_or("repository ruleset response is invalid")?;
    let mut found = serde_json::Map::new();
    for ruleset in listed {
        let name = ruleset.get("name").and_then(Value::as_str).ok_or("repository ruleset entry is invalid")?;
        if found.insert(name.to_owned(), ruleset.clone()).is_some() {
            return Err("repository contains duplicate release rulesets".into());
        }
    }
    if found.keys().map(String::as_str).collect::<BTreeSet<_>>() != RULESETS.iter().map(|(name, _)| *name).collect() {
        return Err("repository release ruleset set is invalid".into());
    }
    let mut ids = Vec::new();
    for (name, label) in RULESETS {
        let ruleset = &found[name];
        let id = ruleset.get("id").filter(|id| id.is_i64() || id.is_u64());
        let (true, Some(id)) = (ruleset.get("enforcement") == Some(&json!("active")), id) else {
            return Err(format!("{name} must be active"));
        };
        // The ID becomes part of an API path.
        let id = id.as_u64().filter(|id| *id > 0).ok_or("repository ruleset identity is invalid")?;
        ids.push((label, id));
    }
    Ok(ids)
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message.to_owned()) }
}

/// A JSON value as a set member (missing is `null`); sets compare values.
fn encoded(value: Option<&Value>) -> String {
    value.map_or_else(|| "null".to_owned(), Value::to_string)
}

/// The `name` fields of a listing's objects (non-objects are skipped).
fn names(items: &[Value]) -> BTreeSet<String> {
    items.iter().filter(|item| item.is_object()).map(|item| encoded(item.get("name"))).collect()
}

fn strings(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| json!(value).to_string()).collect()
}

/// API responses by name (see [`ENDPOINTS`]; `ruleset-LABEL` for rulesets).
struct Responses {
    responses: BTreeMap<String, Value>,
}

impl Responses {
    fn read(&self, name: &str) -> Result<Value, String> {
        self.responses.get(name).cloned().ok_or_else(|| format!("{name} response is missing"))
    }

    /// The list under `key` of an object response.
    fn entries(&self, name: &str, key: &str) -> Result<Vec<Value>, String> {
        let value = self.read(name)?;
        value.get(key).and_then(Value::as_array).cloned().ok_or_else(|| format!("{name} response is invalid"))
    }

    fn ruleset(&self, name: &str, target: &str, includes: &[&str], rule_types: &[&str]) -> Result<Vec<Value>, String> {
        let value = self.read(&format!("ruleset-{name}"))?;
        require(
            value.is_object()
                && value.get("target") == Some(&json!(target))
                && value.get("enforcement") == Some(&json!("active")),
            &format!("{name} ruleset is invalid"),
        )?;
        let references = value.get("conditions").and_then(|conditions| conditions.get("ref_name"));
        require(
            references.is_some_and(|references| {
                references.get("include") == Some(&json!(includes)) && references.get("exclude") == Some(&json!([]))
            }),
            &format!("{name} ruleset target is invalid"),
        )?;
        let rules = value.get("rules").and_then(Value::as_array).cloned();
        let types: Option<BTreeSet<String>> = rules
            .as_ref()
            .map(|rules| rules.iter().filter(|rule| rule.is_object()).map(|rule| encoded(rule.get("type"))).collect());
        require(types == Some(strings(rule_types)), &format!("{name} ruleset rules are invalid"))?;
        require(value.get("bypass_actors") == Some(&json!([])), &format!("{name} ruleset bypass actors are invalid"))?;
        Ok(rules.unwrap_or_default())
    }
}

fn check(repository: &str, responses: &Responses) -> Result<(), String> {
    let owner_login = repository.split('/').next().unwrap_or_default();

    let repo = responses.read("repository")?;
    require(repo.is_object() && repo.get("full_name") == Some(&json!(repository)), "repository identity is invalid")?;
    require(
        repo.get("private") == Some(&json!(false))
            && repo.get("visibility") == Some(&json!("public"))
            && repo.get("archived") == Some(&json!(false)),
        "repository must be active and public",
    )?;
    let owner = repo.get("owner");
    require(
        owner.is_some_and(|owner| {
            owner.is_object()
                && owner.get("id").is_some_and(|id| id.is_i64() || id.is_u64())
                && owner.get("login") == Some(&json!(owner_login))
                && owner.get("type") == Some(&json!("User"))
        }),
        "repository owner identity is invalid",
    )?;
    let security = repo.get("security_and_analysis");
    let enabled = |key: &str| {
        security.and_then(|security| security.get(key)).and_then(|item| item.get("status")) == Some(&json!("enabled"))
    };
    require(
        security.is_some_and(Value::is_object)
            && enabled("secret_scanning")
            && enabled("secret_scanning_push_protection"),
        "secret scanning and push protection must be enabled",
    )?;

    let collaborators = responses.read("collaborators")?;
    let only = collaborators.as_array().filter(|items| items.len() == 1).map(|items| &items[0]);
    require(only.is_some(), "repository must have exactly one collaborator: its owner")?;
    let collaborator = only.expect("checked above");
    require(
        collaborator.is_object()
            && collaborator.get("login") == Some(&json!(owner_login))
            && collaborator.get("role_name") == Some(&json!("admin"))
            && collaborator
                .get("permissions")
                .is_some_and(|permissions| permissions.is_object() && permissions.get("admin") == Some(&json!(true))),
        "repository's only collaborator must be its owner with admin access",
    )?;
    require(responses.read("invitations")? == json!([]), "repository must not have pending invitations")?;
    require(responses.read("deploy-keys")? == json!([]), "repository must not have deploy keys")?;

    let workflows = responses.entries("workflows", "workflows")?;
    let mut observed: Vec<(String, String)> = workflows
        .iter()
        .filter(|item| item.is_object())
        .map(|item| (encoded(item.get("path")), encoded(item.get("state"))))
        .collect();
    observed.sort();
    let expected: Vec<(String, String)> =
        [".github/workflows/ci.yml", ".github/workflows/release-please.yml", ".github/workflows/release.yml"]
            .iter()
            .map(|path| (json!(path).to_string(), json!("active").to_string()))
            .collect();
    require(observed == expected, "only CI, Release Please, and release workflows may be active")?;
    let actions = responses.read("actions-permissions")?;
    require(
        actions.is_object()
            && actions.get("enabled") == Some(&json!(true))
            && actions.get("allowed_actions") == Some(&json!("selected"))
            && actions.get("sha_pinning_required") == Some(&json!(true)),
        "Actions must be enabled, selected, and SHA-pinned",
    )?;
    let selected = responses.read("selected-actions")?;
    let patterns: Option<BTreeSet<String>> = match selected.get("patterns_allowed") {
        None => Some(BTreeSet::new()),
        Some(patterns) => patterns.as_array().map(|patterns| patterns.iter().map(Value::to_string).collect()),
    };
    require(
        selected.is_object()
            && selected.get("github_owned_allowed") == Some(&json!(true))
            && selected.get("verified_allowed") == Some(&json!(false))
            && patterns == Some(strings(&["cachix/install-nix-action@*", "googleapis/release-please-action@*"])),
        "only GitHub-owned Actions, Nix, and Release Please may run",
    )?;
    let workflow = responses.read("workflow-permissions")?;
    require(
        workflow.is_object()
            && workflow.get("default_workflow_permissions") == Some(&json!("read"))
            && workflow.get("can_approve_pull_request_reviews") == Some(&json!(false)),
        "default GITHUB_TOKEN permissions must be read-only",
    )?;
    require(
        responses.read("fork-approval")? == json!({"approval_policy": "all_external_contributors"}),
        "all external fork workflows must require approval",
    )?;
    require(
        responses.entries("runners", "runners")?.is_empty(),
        "keyless hosted releases must not use repository self-hosted runners",
    )?;

    require(
        names(&responses.entries("variables", "variables")?).is_empty(),
        "keyless hosted releases must not depend on repository variables",
    )?;
    require(
        names(&responses.entries("repository-secrets", "secrets")?) == strings(&["RELEASE_PLEASE_TOKEN"]),
        "repository secrets must contain only RELEASE_PLEASE_TOKEN",
    )?;

    require(
        names(&responses.entries("environments", "environments")?) == strings(&["hamn-promotion"]),
        "hamn-promotion must be the only release environment",
    )?;
    let promotion = responses.read("promotion")?;
    require(
        promotion.is_object()
            && promotion.get("name") == Some(&json!("hamn-promotion"))
            && promotion.get("can_admins_bypass") == Some(&json!(false))
            && promotion.get("deployment_branch_policy")
                == Some(&json!({"protected_branches": false, "custom_branch_policies": true})),
        "hamn-promotion must be fail-closed and use custom branch policies",
    )?;
    let protection = promotion.get("protection_rules").and_then(Value::as_array);
    require(
        protection.is_some_and(|rules| rules.len() == 1 && rules[0].get("type") == Some(&json!("branch_policy"))),
        "hamn-promotion must enforce its branch policy",
    )?;
    let branches = responses.entries("promotion-branches", "branch_policies")?;
    require(
        branches.len() == 1
            && branches[0].get("name") == Some(&json!("main"))
            && branches[0].get("type") == Some(&json!("branch")),
        "hamn-promotion must allow only the main branch",
    )?;
    require(
        names(&responses.entries("promotion-secrets", "secrets")?).is_empty()
            && names(&responses.entries("promotion-variables", "variables")?).is_empty(),
        "hamn-promotion must not contain secrets or variables",
    )?;

    let main_rules = responses.ruleset(
        "main",
        "branch",
        &["~DEFAULT_BRANCH"],
        &["deletion", "non_fast_forward", "required_linear_history", "pull_request", "required_status_checks"],
    )?;
    let rule = |kind: &str| {
        main_rules.iter().find(|rule| rule.get("type") == Some(&json!(kind))).cloned().unwrap_or(Value::Null)
    };
    let parameters = rule("pull_request").get("parameters").cloned();
    let unattributed = "require_extra_approval_for_unattributed_changes";
    let pull_request_safe = parameters.as_ref().and_then(Value::as_object).is_some_and(|parameters| {
        let optional_flag = parameters.get(unattributed).is_none_or(Value::is_boolean);
        let mut rest = parameters.clone();
        rest.remove(unattributed);
        optional_flag
            && Value::Object(rest)
                == json!({
                    "required_approving_review_count": 0,
                    "dismiss_stale_reviews_on_push": false,
                    "required_reviewers": [],
                    "require_code_owner_review": false,
                    "require_last_push_approval": false,
                    "required_review_thread_resolution": true,
                    "allowed_merge_methods": ["squash", "rebase"],
                })
    });
    require(pull_request_safe, "main pull request rules are not solo-maintainer safe")?;
    let parameters = rule("required_status_checks").get("parameters").cloned().unwrap_or(Value::Null);
    let contexts: BTreeSet<String> = parameters
        .get("required_status_checks")
        .and_then(Value::as_array)
        .map(|checks| {
            checks.iter().filter(|check| check.is_object()).map(|check| encoded(check.get("context"))).collect()
        })
        .unwrap_or_default();
    require(
        parameters.is_object()
            && parameters.get("strict_required_status_checks_policy") == Some(&json!(true))
            && contexts == strings(&["Portable source gates", "macOS build and regression gates"]),
        "main must require both Nix CI status checks on the latest commit",
    )?;
    responses.ruleset("stable-immutable", "tag", &["refs/tags/v*"], &["deletion", "non_fast_forward"])?;

    require(
        responses.read("immutable-releases")?.get("enabled") == Some(&json!(true)),
        "immutable releases must be enabled",
    )?;
    require(
        responses.read("private-vulnerability-reporting")? == json!({"enabled": true}),
        "private vulnerability reporting must be enabled",
    )
}
