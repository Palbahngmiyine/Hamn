//! `release preflight-repository` permits only the pinned CI and release
//! automation state, reads it only with `gh api` GETs, and fails closed on
//! any unsafe setting or unreadable response. `gh` is this executable's
//! `release-preflight-gh` fixture, which answers from recorded responses
//! of `example/hamn`; the real GitHub API is never called.
use crate::runner::{self, case};
use crate::support::release_driver::{Outcome, Repo, hamn_dev, pairs, private_bin, text, with};
use std::fs;
use std::io::Write;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-repository-preflight",
        "automated release repository preflight is read-only and fail-closed",
        vec![
            case("pinned_state_passes_with_only_api_reads", pinned_state_passes_with_only_api_reads),
            case("each_unsafe_setting_fails_closed", each_unsafe_setting_fails_closed),
            case("current_repository_comes_from_gh", current_repository_comes_from_gh),
            case("invalid_repository_or_missing_gh_calls_nothing", invalid_repository_or_missing_gh_calls_nothing),
            case("unreadable_responses_and_ruleset_ids_fail", unreadable_responses_and_ruleset_ids_fail),
        ],
        filters,
    )
}

const REPOSITORY: &str = "repos/example/hamn";
const PASSED: &str = "automated release repository preflight passed for example/hamn\n";

struct Preflight {
    repo: Repo,
    environment: Vec<(String, String)>,
}

impl Preflight {
    fn new() -> Self {
        let repo = Repo::new("hamn-release-preflight-");
        let bin = private_bin(&repo.scratch(""), &[], &["gh"]);
        let log = repo.scratch("gh.log");
        fs::write(&log, "").unwrap();
        let environment = vec![
            ("PATH".into(), text(&bin)),
            ("HAMN_DEV_FIXTURE".into(), "release-preflight-gh".into()),
            ("HAMN_TEST_GH_LOG".into(), text(&log)),
            ("HAMN_RELEASE_REPOSITORY".into(), "example/hamn".into()),
        ];
        Self { repo, environment }
    }

    fn run(&self, environment: &[(String, String)]) -> Outcome {
        hamn_dev(&["release", "preflight-repository"], self.repo.root(), &pairs(environment))
    }

    fn toggled(&self, variable: &str) -> Vec<(String, String)> {
        with(&self.environment, variable, Some("1"))
    }

    /// The recorded `gh` invocations, one argument list per line.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.repo.scratch("gh.log")).unwrap().lines().map(str::to_owned).collect()
    }
}

fn pinned_state_passes_with_only_api_reads() {
    let preflight = Preflight::new();
    let outcome = preflight.run(&preflight.environment);
    assert_eq!(outcome.succeeded().stdout, PASSED);
    let calls = preflight.calls();
    assert_eq!(calls.len(), 22, "{calls:#?}");
    for call in &calls {
        let words: Vec<&str> = call.split('\t').collect();
        // `gh api ENDPOINT` without a method, field or input is a GET.
        assert!(
            words.len() == 2 && words[0] == "api" && words[1].starts_with(REPOSITORY),
            "not a plain API read: {call}"
        );
    }
    for ruleset in ["repos/example/hamn/rulesets/1", "repos/example/hamn/rulesets/2"] {
        assert!(calls.contains(&format!("api\t{ruleset}")), "{ruleset} was not read");
    }
}

fn each_unsafe_setting_fails_closed() {
    let preflight = Preflight::new();
    for (variable, message) in [
        ("HAMN_TEST_WEAK_ACTIONS_POLICY", "Actions must be enabled, selected, and SHA-pinned"),
        ("HAMN_TEST_UNSAFE_ACTION", "only GitHub-owned Actions, Nix, and Release Please may run"),
        ("HAMN_TEST_RELEASE_PLEASE_SECRET", "repository secrets must contain only RELEASE_PLEASE_TOKEN"),
        ("HAMN_TEST_RUNNER", "keyless hosted releases must not use repository self-hosted runners"),
        ("HAMN_TEST_VARIABLE", "keyless hosted releases must not depend on repository variables"),
        ("HAMN_TEST_SECRET", "hamn-promotion must not contain secrets or variables"),
        ("HAMN_TEST_BRANCH_POLICY", "hamn-promotion must allow only the main branch"),
        ("HAMN_TEST_WEAK_RULESET", "main pull request rules are not solo-maintainer safe"),
        ("HAMN_TEST_COLLABORATOR", "repository must have exactly one collaborator: its owner"),
        ("HAMN_TEST_INVITATION", "repository must not have pending invitations"),
        ("HAMN_TEST_DEPLOY_KEY", "repository must not have deploy keys"),
        ("HAMN_TEST_INACTIVE_RULESET", "immutable-stable-releases must be active"),
    ] {
        let outcome = preflight.run(&preflight.toggled(variable));
        outcome.failed_with(&format!("release repository preflight: {message}"));
        assert!(!outcome.stdout.contains("passed"), "{variable}: {outcome:#?}");
    }
}

fn current_repository_comes_from_gh() {
    let preflight = Preflight::new();
    let unset = with(&preflight.environment, "HAMN_RELEASE_REPOSITORY", None);
    assert_eq!(preflight.run(&unset).succeeded().stdout, PASSED);
    assert_eq!(preflight.calls()[0], "repo\tview\t--json\tnameWithOwner\t--jq\t.nameWithOwner");
    let failing = with(&unset, "HAMN_TEST_REPO_VIEW_FAIL", Some("1"));
    preflight.run(&failing).failed_with("cannot resolve the current GitHub repository");
}

fn invalid_repository_or_missing_gh_calls_nothing() {
    let preflight = Preflight::new();
    for repository in ["example", "example/hamn/extra", "exa mple/hamn", "example/"] {
        let environment = with(&preflight.environment, "HAMN_RELEASE_REPOSITORY", Some(repository));
        preflight.run(&environment).failed_with("HAMN_RELEASE_REPOSITORY must be owner/repository");
    }
    let empty_bin = preflight.repo.scratch("empty-bin");
    fs::create_dir(&empty_bin).unwrap();
    let without_gh = with(&preflight.environment, "PATH", Some(&text(&empty_bin)));
    preflight.run(&without_gh).failed_with("GitHub CLI (gh) is required");
    let unset = with(&without_gh, "HAMN_RELEASE_REPOSITORY", None);
    preflight.run(&unset).failed_with("GitHub CLI (gh) is required");
    assert!(preflight.calls().is_empty(), "{:#?}", preflight.calls());
}

fn unreadable_responses_and_ruleset_ids_fail() {
    let preflight = Preflight::new();
    let failing = with(&preflight.environment, "HAMN_TEST_GH_FAIL", Some("repos/example/hamn/keys"));
    preflight.run(&failing).failed_with("cannot read repository deploy keys");
    let failing = with(&preflight.environment, "HAMN_TEST_GH_FAIL", Some("repos/example/hamn/rulesets/2"));
    preflight.run(&failing).failed_with("cannot read stable-immutable ruleset");
    let invalid = with(&preflight.environment, "HAMN_TEST_GH_INVALID", Some("repos/example/hamn/actions/runners"));
    preflight.run(&invalid).failed_with("repository runners response is not valid JSON");
    for id in ["0", "-1"] {
        let environment = with(&preflight.environment, "HAMN_TEST_RULESET_ID", Some(id));
        preflight.run(&environment).failed_with("repository ruleset identity is invalid");
    }
    let environment = with(&preflight.environment, "HAMN_TEST_RULESET_ID", Some("\"1\""));
    preflight.run(&environment).failed_with("protect-main-and-release-workflow must be active");
}

/// Fixture `gh`: logs each invocation (tab-separated) to
/// `$HAMN_TEST_GH_LOG`, answers `repo view` with example/hamn and
/// `api ENDPOINT` with the recorded response, changed by the
/// `HAMN_TEST_*` toggles. Unknown calls exit 64 or 65.
pub fn gh(_program: &str, args: &[String]) -> ExitCode {
    let toggle = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
    if let Ok(log) = std::env::var("HAMN_TEST_GH_LOG") {
        let mut file = fs::OpenOptions::new().append(true).open(log).expect("gh log");
        writeln!(file, "{}", args.join("\t")).expect("gh log");
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let endpoint = match words.as_slice() {
        ["repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"] => {
            if toggle("HAMN_TEST_REPO_VIEW_FAIL") {
                eprintln!("gh: not a GitHub repository");
                return ExitCode::FAILURE;
            }
            println!("example/hamn");
            return ExitCode::SUCCESS;
        }
        ["api", endpoint] => *endpoint,
        _ => return ExitCode::from(64),
    };
    if std::env::var("HAMN_TEST_GH_FAIL").is_ok_and(|failing| failing == endpoint) {
        eprintln!("gh: HTTP 403");
        return ExitCode::FAILURE;
    }
    if std::env::var("HAMN_TEST_GH_INVALID").is_ok_and(|invalid| invalid == endpoint) {
        println!("{{");
        return ExitCode::SUCCESS;
    }
    let Some(path) = endpoint.strip_prefix(REPOSITORY) else {
        return ExitCode::from(65);
    };
    let choose = |name: &str, unsafe_response: &'static str, safe: &'static str| {
        if toggle(name) { unsafe_response } else { safe }
    };
    let ruleset_id = std::env::var("HAMN_TEST_RULESET_ID").unwrap_or_else(|_| "1".into());
    let rulesets = format!(
        "[{{\"id\":{ruleset_id},\"name\":\"protect-main-and-release-workflow\",\"enforcement\":\"active\"}},\
         {{\"id\":2,\"name\":\"immutable-stable-releases\",\"enforcement\":\"{}\"}}]",
        if toggle("HAMN_TEST_INACTIVE_RULESET") { "evaluate" } else { "active" }
    );
    let response = match path {
        "" => {
            r#"{"full_name":"example/hamn","private":false,"visibility":"public","archived":false,"owner":{"id":42,"login":"example","type":"User"},"security_and_analysis":{"secret_scanning":{"status":"enabled"},"secret_scanning_push_protection":{"status":"enabled"}}}"#
        }
        "/collaborators?affiliation=all&per_page=100" => choose(
            "HAMN_TEST_COLLABORATOR",
            r#"[{"login":"example","role_name":"admin","permissions":{"admin":true}},{"login":"outsider","role_name":"write","permissions":{"admin":false}}]"#,
            r#"[{"login":"example","role_name":"admin","permissions":{"admin":true}}]"#,
        ),
        "/invitations" => choose("HAMN_TEST_INVITATION", r#"[{"id":7,"invitee":{"login":"pending-user"}}]"#, "[]"),
        "/keys" => choose("HAMN_TEST_DEPLOY_KEY", r#"[{"id":8,"title":"unexpected","read_only":false}]"#, "[]"),
        "/actions/workflows" => {
            r#"{"workflows":[{"path":".github/workflows/release.yml","state":"active"},{"path":".github/workflows/ci.yml","state":"active"},{"path":".github/workflows/release-please.yml","state":"active"}]}"#
        }
        "/actions/permissions" => choose(
            "HAMN_TEST_WEAK_ACTIONS_POLICY",
            r#"{"enabled":true,"allowed_actions":"all"}"#,
            r#"{"enabled":true,"allowed_actions":"selected","sha_pinning_required":true}"#,
        ),
        "/actions/permissions/selected-actions" => choose(
            "HAMN_TEST_UNSAFE_ACTION",
            r#"{"github_owned_allowed":true,"verified_allowed":true,"patterns_allowed":[]}"#,
            r#"{"github_owned_allowed":true,"verified_allowed":false,"patterns_allowed":["googleapis/release-please-action@*","cachix/install-nix-action@*"]}"#,
        ),
        "/actions/permissions/workflow" => {
            r#"{"default_workflow_permissions":"read","can_approve_pull_request_reviews":false}"#
        }
        "/actions/permissions/fork-pr-contributor-approval" => r#"{"approval_policy":"all_external_contributors"}"#,
        "/actions/runners" => {
            choose("HAMN_TEST_RUNNER", r#"{"runners":[{"name":"unsafe-runner"}]}"#, r#"{"runners":[]}"#)
        }
        "/actions/variables" => {
            choose("HAMN_TEST_VARIABLE", r#"{"variables":[{"name":"UNTRUSTED_RELEASE_INPUT"}]}"#, r#"{"variables":[]}"#)
        }
        "/actions/secrets" => choose(
            "HAMN_TEST_RELEASE_PLEASE_SECRET",
            r#"{"secrets":[]}"#,
            r#"{"secrets":[{"name":"RELEASE_PLEASE_TOKEN"}]}"#,
        ),
        "/environments" => r#"{"environments":[{"name":"hamn-promotion"}]}"#,
        "/environments/hamn-promotion" => {
            r#"{"id":7,"name":"hamn-promotion","can_admins_bypass":false,"protection_rules":[{"id":8,"type":"branch_policy"}],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}"#
        }
        "/environments/hamn-promotion/secrets" => {
            choose("HAMN_TEST_SECRET", r#"{"secrets":[{"name":"HAMN_RELEASE_SIGNING_KEY"}]}"#, r#"{"secrets":[]}"#)
        }
        "/environments/hamn-promotion/variables" => r#"{"variables":[]}"#,
        "/environments/hamn-promotion/deployment-branch-policies" => choose(
            "HAMN_TEST_BRANCH_POLICY",
            r#"{"branch_policies":[{"name":"release/*","type":"branch"}]}"#,
            r#"{"branch_policies":[{"name":"main","type":"branch"}]}"#,
        ),
        "/rulesets" => {
            println!("{rulesets}");
            return ExitCode::SUCCESS;
        }
        "/rulesets/1" => choose(
            "HAMN_TEST_WEAK_RULESET",
            r#"{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"},{"type":"required_linear_history"},{"type":"pull_request","parameters":{"required_approving_review_count":1,"dismiss_stale_reviews_on_push":true,"required_reviewers":[],"require_code_owner_review":false,"require_last_push_approval":true,"required_review_thread_resolution":true,"allowed_merge_methods":["squash","rebase"]}},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":false,"required_status_checks":[]}}],"bypass_actors":[]}"#,
            r#"{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"},{"type":"required_linear_history"},{"type":"pull_request","parameters":{"require_extra_approval_for_unattributed_changes":true,"required_approving_review_count":0,"dismiss_stale_reviews_on_push":false,"required_reviewers":[],"require_code_owner_review":false,"require_last_push_approval":false,"required_review_thread_resolution":true,"allowed_merge_methods":["squash","rebase"]}},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":true,"required_status_checks":[{"context":"Portable source gates"},{"context":"macOS build and regression gates"}]}}],"bypass_actors":[]}"#,
        ),
        "/rulesets/2" => {
            r#"{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v*"],"exclude":[]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}"#
        }
        "/immutable-releases" => r#"{"enabled":true,"enforced_by_owner":false}"#,
        "/private-vulnerability-reporting" => r#"{"enabled":true}"#,
        _ => return ExitCode::from(65),
    };
    println!("{response}");
    ExitCode::SUCCESS
}
