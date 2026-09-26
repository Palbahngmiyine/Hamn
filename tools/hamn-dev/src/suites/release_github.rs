//! Release Please coordination against recorded `gh` and `git` responses:
//! release PRs complete only for their exact, published, immutable version,
//! and new release notes wait until the current version is published.
use crate::release::github::{Commands, complete, ready};
use crate::release::process::Output;
use crate::runner::{self, case};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::os::unix::process::ExitStatusExt;
use std::process::{ExitCode, ExitStatus};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-github",
        "release PR completion and publication readiness",
        vec![
            case("published_ancestor_completes_only_pending_label", published_ancestor_completes_only_pending_label),
            case(
                "wrong_version_unrelated_commit_and_already_completed_are_untouched",
                wrong_version_unrelated_commit_and_already_completed_are_untouched,
            ),
            case(
                "unpublished_mutable_wrong_tag_or_wrong_commit_cannot_complete",
                unpublished_mutable_wrong_tag_or_wrong_commit_cannot_complete,
            ),
            case(
                "git_failure_and_invalid_listings_do_not_clear_pending",
                git_failure_and_invalid_listings_do_not_clear_pending,
            ),
            case("invalid_identity_cannot_call_github", invalid_identity_cannot_call_github),
            case(
                "manifest_merge_defers_until_exact_release_is_published",
                manifest_merge_defers_until_exact_release_is_published,
            ),
            case("gh_crlf_headers_and_plain_lf_both_parse", gh_crlf_headers_and_plain_lf_both_parse),
            case(
                "api_and_network_failures_do_not_look_like_pending_publication",
                api_and_network_failures_do_not_look_like_pending_publication,
            ),
            case("invalid_or_wrong_release_is_rejected", invalid_or_wrong_release_is_rejected),
            case("invalid_configuration_cannot_call_the_api", invalid_configuration_cannot_call_the_api),
        ],
        filters,
    )
}

const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MERGE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn exited(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

type Reply = Result<Output, String>;

/// Answers each command through `respond` and records every call.
struct Recorded<F: Fn(&str, &[String]) -> Reply> {
    respond: F,
    calls: RefCell<Vec<(String, Vec<String>, Duration)>>,
}

impl<F: Fn(&str, &[String]) -> Reply> Recorded<F> {
    fn new(respond: F) -> Self {
        Self { respond, calls: RefCell::new(Vec::new()) }
    }

    /// The argument lists of calls that start with `program prefix...`.
    fn calls_to(&self, program: &str, prefix: &[&str]) -> Vec<Vec<String>> {
        self.calls
            .borrow()
            .iter()
            .filter(|(called, args, _)| called == program && args.iter().zip(prefix).all(|(arg, word)| arg == word))
            .map(|(_, args, _)| args.clone())
            .collect()
    }
}

impl<F: Fn(&str, &[String]) -> Reply> Commands for Recorded<F> {
    fn run(&self, program: &str, args: &[String], timeout: Duration) -> Reply {
        self.calls.borrow_mut().push((program.to_owned(), args.to_vec(), timeout));
        (self.respond)(program, args)
    }
}

fn published_release() -> Value {
    json!({"tagName": "v0.1.0", "targetCommitish": COMMIT, "isDraft": false, "isPrerelease": false, "isImmutable": true})
}

struct Completion {
    release: Value,
    version: &'static str,
    ancestor: Option<i32>,
    pending: Value,
}

impl Default for Completion {
    fn default() -> Self {
        Self {
            release: published_release(),
            version: "0.1.0",
            ancestor: Some(0),
            pending: json!([{"number": 43, "mergeCommit": {"oid": MERGE}}]),
        }
    }
}

impl Completion {
    /// Runs completion; returns its result and the `gh pr edit` calls.
    fn exercise(self) -> (Result<Vec<u64>, String>, Vec<Vec<String>>) {
        let commands = Recorded::new(|program: &str, args: &[String]| {
            let words: Vec<&str> = args.iter().map(String::as_str).collect();
            match (program, words.as_slice()) {
                ("gh", ["release", "view", ..]) => Ok(exited(0, &self.release.to_string(), "")),
                ("gh", ["pr", "list", ..]) => Ok(exited(0, &self.pending.to_string(), "")),
                ("git", ["merge-base", "--is-ancestor", ..]) => Ok(match self.ancestor {
                    Some(code) => exited(code, "", ""),
                    None => {
                        Output { status: ExitStatus::from_raw(libc::SIGKILL), stdout: Vec::new(), stderr: Vec::new() }
                    }
                }),
                ("git", ["show", object]) => {
                    assert_eq!(*object, format!("{MERGE}:.release-please-manifest.json"));
                    Ok(exited(0, &json!({".": self.version}).to_string(), ""))
                }
                ("gh", ["pr", "edit", ..]) => Ok(exited(0, "", "")),
                _ => panic!("unexpected command {program} {args:?}"),
            }
        });
        let result = complete(&commands, "example/hamn", "v0.1.0", COMMIT);
        (result, commands.calls_to("gh", &["pr", "edit"]))
    }
}

fn published_ancestor_completes_only_pending_label() {
    let (result, edits) = Completion::default().exercise();
    assert_eq!(result.unwrap(), [43]);
    assert_eq!(edits, [["pr", "edit", "43", "--repo", "example/hamn", "--remove-label", "autorelease: pending"]]);
}

fn wrong_version_unrelated_commit_and_already_completed_are_untouched() {
    for completion in [
        Completion { version: "0.2.0", ..Completion::default() },
        Completion { ancestor: Some(1), ..Completion::default() },
        Completion { pending: json!([]), ..Completion::default() },
    ] {
        let (result, edits) = completion.exercise();
        assert_eq!(result.unwrap(), Vec::<u64>::new());
        assert!(edits.is_empty(), "{edits:?}");
    }
}

fn unpublished_mutable_wrong_tag_or_wrong_commit_cannot_complete() {
    for (key, value) in [
        ("isDraft", json!(true)),
        ("isPrerelease", json!(true)),
        ("isImmutable", json!(false)),
        ("isImmutable", Value::Null),
        ("tagName", json!("v0.2.0")),
        ("targetCommitish", json!(MERGE)),
        ("unexpected", json!(true)),
    ] {
        let mut release = published_release();
        release[key] = value.clone();
        let (result, edits) = Completion { release, ..Completion::default() }.exercise();
        assert!(result.unwrap_err().contains("immutable, published"), "{key}={value}");
        assert!(edits.is_empty());
    }
}

fn git_failure_and_invalid_listings_do_not_clear_pending() {
    for completion in [
        Completion { ancestor: Some(128), ..Completion::default() },
        Completion { ancestor: None, ..Completion::default() },
        Completion { pending: json!({"number": 43}), ..Completion::default() },
        Completion {
            pending: Value::Array(vec![json!({"number": 1, "mergeCommit": {"oid": MERGE}}); 1000]),
            ..Completion::default()
        },
        Completion { pending: json!([{"number": 0, "mergeCommit": {"oid": MERGE}}]), ..Completion::default() },
        Completion { pending: json!([{"number": "43", "mergeCommit": {"oid": MERGE}}]), ..Completion::default() },
        Completion { pending: json!([{"number": 43.0, "mergeCommit": {"oid": MERGE}}]), ..Completion::default() },
        Completion { pending: json!([{"number": 43, "mergeCommit": {"oid": "b"}}]), ..Completion::default() },
        Completion { pending: json!([{"number": 43, "mergeCommit": null}]), ..Completion::default() },
    ] {
        let pending = completion.pending.to_string();
        let (result, edits) = completion.exercise();
        assert!(result.is_err(), "{pending}");
        assert!(edits.is_empty(), "{pending}");
    }
}

fn invalid_identity_cannot_call_github() {
    for (repository, tag, commit) in [
        ("example/hamn/../../other", "v0.1.0", COMMIT),
        ("example/hamn", "0.1.0", COMMIT),
        ("example/hamn", "v01.0.0", COMMIT),
        ("example/hamn", "v0.1.0-rc.1", COMMIT),
        ("example/hamn", "v0.1.0", &COMMIT[1..]),
        ("example/hamn", "v0.1.0", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
    ] {
        let commands = Recorded::new(|program: &str, args: &[String]| panic!("called {program} {args:?}"));
        assert_eq!(complete(&commands, repository, tag, commit).unwrap_err(), "invalid release identity");
    }
}

fn api_response(status: u16, code: i32, fields: &[(&str, Value)]) -> Reply {
    let mut release = json!({"tag_name": "v0.1.0", "draft": false, "prerelease": false, "immutable": true});
    for (key, value) in fields {
        release[*key] = value.clone();
    }
    Ok(exited(code, &gh_include(status, &release.to_string()), ""))
}

/// `gh api --include` output as gh 2.x writes it: the status line ends with
/// LF, each header and the blank line after them with CRLF.
fn gh_include(status: u16, body: &str) -> String {
    format!(
        "HTTP/2.0 {status} Status\nContent-Type: application/json; charset=utf-8\r\n\
         X-Github-Api-Version-Selected: 2022-11-28\r\n\r\n{body}"
    )
}

/// Readiness with one recorded API reply; asserts the exact request.
fn readiness(reply: Reply) -> Result<bool, String> {
    let reply = RefCell::new(Some(reply));
    let commands = Recorded::new(|_: &str, _: &[String]| reply.borrow_mut().take().expect("one API call"));
    let result = ready(&commands, "example/hamn", &json!({".": "0.1.0"}));
    let calls = commands.calls.borrow();
    let expected = ["api", "--include", "repos/example/hamn/releases/tags/v0.1.0"].map(String::from).to_vec();
    assert_eq!(*calls, [("gh".to_owned(), expected, Duration::from_secs(30))]);
    result
}

fn manifest_merge_defers_until_exact_release_is_published() {
    assert!(!readiness(api_response(404, 1, &[])).unwrap());
    assert!(!readiness(api_response(200, 0, &[("draft", json!(true)), ("immutable", json!(false))])).unwrap());
    assert!(readiness(api_response(200, 0, &[])).unwrap());
}

/// Real gh output has no LF LF: headers and the separating blank line end
/// with CRLF (Release Please run 36234002500 failed on it). Plain LF, which
/// the Python original also accepted, keeps working.
fn gh_crlf_headers_and_plain_lf_both_parse() {
    let release = json!({"tag_name": "v0.1.0", "draft": false, "prerelease": false, "immutable": true});
    let crlf = gh_include(200, &release.to_string());
    assert!(!crlf.contains("\n\n") && crlf.contains("\r\n\r\n"), "{crlf:?}");
    assert!(readiness(Ok(exited(0, &crlf, ""))).unwrap());
    assert!(!readiness(Ok(exited(1, &gh_include(404, "{\"message\":\"Not Found\"}"), ""))).unwrap());
    let lf = format!("HTTP/2.0 200 OK\nContent-Type: application/json\n\n{release}");
    assert!(readiness(Ok(exited(0, &lf, ""))).unwrap());
    // A header block with no blank line after it is still not a response.
    assert!(readiness(Ok(exited(0, "HTTP/2.0 200 OK\nContent-Type: application/json\r\n", ""))).is_err());
}

fn api_and_network_failures_do_not_look_like_pending_publication() {
    for status in [401, 403, 429, 500, 503] {
        assert!(readiness(api_response(status, 1, &[])).is_err(), "{status}");
    }
    // A 404 needs gh's failure status too; a 404 reported as success is not
    // an understood answer.
    assert!(readiness(api_response(404, 0, &[])).is_err());
    assert!(readiness(Ok(exited(1, "", "connection failed"))).unwrap_err().contains("invalid HTTP response"));
    assert!(readiness(Err("gh timed out after 30s".into())).unwrap_err().contains("timed out"));
}

fn invalid_or_wrong_release_is_rejected() {
    for (key, value) in [
        ("tag_name", json!("v0.0.1")),
        ("draft", Value::Null),
        ("prerelease", json!(true)),
        ("immutable", json!(false)),
        ("immutable", Value::Null),
    ] {
        assert!(readiness(api_response(200, 0, &[(key, value.clone())])).is_err(), "{key}={value}");
    }
    assert!(readiness(api_response(202, 0, &[])).unwrap_err().contains("unexpected status"));
    assert!(readiness(Ok(exited(0, "HTTP/2.0 200 OK\n\n{", ""))).is_err());
    assert!(readiness(Ok(exited(0, "HTTP/2.0 200 OK\n\n[]", ""))).is_err());
}

fn invalid_configuration_cannot_call_the_api() {
    let commands = Recorded::new(|program: &str, args: &[String]| panic!("called {program} {args:?}"));
    for manifest in [
        Value::Null,
        json!({}),
        json!({".": null}),
        json!({".": "0.1.0-rc.1"}),
        json!({".": "01.0.0"}),
        json!({".": "0.1.0", "other": "0.1.0"}),
        json!({".": "../tag"}),
        json!(["0.1.0"]),
    ] {
        assert!(ready(&commands, "example/hamn", &manifest).is_err(), "{manifest}");
    }
    assert!(ready(&commands, "example/hamn/../../other", &json!({".": "0.1.0"})).is_err());
    assert!(commands.calls.borrow().is_empty());
}
