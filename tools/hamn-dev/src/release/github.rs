//! Release Please coordination through the GitHub CLI.
//!
//! - `complete-pr TAG COMMIT` clears `autorelease: pending` from merged
//!   release PRs only after their exact version is an immutable, published
//!   release at this commit.
//! - `pr-ready` defers new release notes until the manifest's current tag is
//!   published, so Release Please never compares against a missing tag.
//!
//! An API, authentication or network failure is always an error, never
//! "pending": a pending result must be proven by a 404 or a draft release.
use super::process::{self, Output, Spec};
use super::syntax::{Version, is_hex, is_repository, is_stable_tag};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Runs `gh` and `git`; tests substitute recorded responses.
pub trait Commands {
    fn run(&self, program: &str, args: &[String], timeout: Duration) -> Result<Output, String>;
}

pub struct System;

impl Commands for System {
    fn run(&self, program: &str, args: &[String], timeout: Duration) -> Result<Output, String> {
        process::capture(OsStr::new(program), args, &Spec::default(), timeout)
    }
}

/// No `gh` or `git` call here is expected to take longer.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
const PENDING_LABEL: &str = "autorelease: pending";

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

/// Standard output of a command that must succeed.
fn output(commands: &dyn Commands, program: &str, args: &[&str]) -> Result<String, String> {
    let result = commands.run(program, &words(args), COMMAND_TIMEOUT)?;
    if !result.status.success() {
        return Err(format!(
            "{program} {} failed: {:?}: {}",
            args.join(" "),
            result.code(),
            result.stderr_lossy().trim()
        ));
    }
    result.stdout_text()
}

fn parse(text: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|error| format!("{what} is not valid JSON: {error}"))
}

/// Returns the numbers of the release PRs whose label was removed.
pub fn complete(commands: &dyn Commands, repository: &str, tag: &str, commit: &str) -> Result<Vec<u64>, String> {
    if !is_repository(repository) || !is_stable_tag(tag) || !is_hex(commit, 40) {
        return Err("invalid release identity".into());
    }
    let fields = "tagName,targetCommitish,isDraft,isPrerelease,isImmutable";
    let release =
        parse(&output(commands, "gh", &["release", "view", tag, "--repo", repository, "--json", fields])?, "release")?;
    let expected = json!({"tagName": tag, "targetCommitish": commit, "isDraft": false, "isPrerelease": false, "isImmutable": true});
    if release != expected {
        return Err("release must be immutable, published, and bound to this commit".into());
    }
    let listing = output(
        commands,
        "gh",
        &[
            "pr",
            "list",
            "--repo",
            repository,
            "--state",
            "merged",
            "--base",
            "main",
            "--label",
            PENDING_LABEL,
            "--limit",
            "1000",
            "--json",
            "number,mergeCommit",
        ],
    )?;
    let pending = parse(&listing, "pending release PR listing")?;
    let pending =
        pending.as_array().filter(|items| items.len() < 1000).ok_or("pending release PR listing is incomplete")?;
    let mut completed = Vec::new();
    for pr in pending {
        let number = pr.get("number").and_then(Value::as_u64).filter(|number| *number > 0);
        let merge = pr.get("mergeCommit").and_then(|commit| commit.get("oid")).and_then(Value::as_str);
        let (Some(number), Some(merge)) = (number, merge.filter(|merge| is_hex(merge, 40))) else {
            return Err("invalid release PR identity".into());
        };
        let ancestor = commands.run("git", &words(&["merge-base", "--is-ancestor", merge, commit]), COMMAND_TIMEOUT)?;
        match ancestor.code() {
            Some(0) => {}
            Some(1) => continue,
            _ => return Err(format!("git merge-base --is-ancestor {merge} {commit} failed: {}", ancestor.status)),
        }
        let manifest =
            parse(&output(commands, "git", &["show", &format!("{merge}:.release-please-manifest.json")])?, "manifest")?;
        if manifest != json!({".": &tag[1..]}) {
            continue;
        }
        // Removing only this label preserves other labels and is idempotent.
        output(
            commands,
            "gh",
            &["pr", "edit", &number.to_string(), "--repo", repository, "--remove-label", PENDING_LABEL],
        )?;
        println!("Release PR #{number} completed for {tag}");
        completed.push(number);
    }
    Ok(completed)
}

/// `complete-pr TAG COMMIT`, for `$GITHUB_REPOSITORY`.
pub fn complete_command(args: &[String]) -> Result<(), String> {
    let [tag, commit] = args else {
        return Err("usage: hamn-dev release complete-pr TAG COMMIT".into());
    };
    let repository = std::env::var("GITHUB_REPOSITORY").map_err(|_| "GITHUB_REPOSITORY is required")?;
    complete(&System, &repository, tag, commit).map(drop)
}

/// Whether the manifest's current version is published: `false` while the
/// release is missing (404) or still a draft.
pub fn ready(commands: &dyn Commands, repository: &str, manifest: &Value) -> Result<bool, String> {
    if !is_repository(repository) || !manifest.is_object() {
        return Err("invalid release identity".into());
    }
    let version = manifest
        .as_object()
        .filter(|object| object.len() == 1)
        .and_then(|object| object.get("."))
        .and_then(Value::as_str);
    let Some(version) = version.filter(|version| Version::parse(version).is_some()) else {
        return Err("invalid release version".into());
    };
    let tag = format!("v{version}");
    let result = commands.run(
        "gh",
        &words(&["api", "--include", &format!("repos/{repository}/releases/tags/{tag}")]),
        Duration::from_secs(30),
    )?;
    // gh ends the status line with LF but each header and the blank line
    // after them with CRLF. Read the output as text, like the Python original
    // did: universal newlines turn CRLF and a lone CR into LF.
    let text = result.stdout_text()?.replace("\r\n", "\n").replace('\r', "\n");
    let (headers, body) = text.split_once("\n\n").ok_or("release API returned an invalid HTTP response")?;
    let status = http_status(headers).ok_or("release API returned an invalid HTTP response")?;
    if status == "404" && result.code() == Some(1) {
        return Ok(false);
    }
    if !result.status.success() {
        return Err(format!("gh api failed ({}): HTTP {status}: {}", result.status, result.stderr_lossy().trim()));
    }
    if status != "200" {
        return Err("release API returned an unexpected status".into());
    }
    let release: Value =
        serde_json::from_str(body).map_err(|error| format!("release API returned invalid JSON: {error}"))?;
    let draft = release.get("draft").and_then(Value::as_bool);
    let (true, Some(draft)) = (release.get("tag_name") == Some(&json!(tag)), draft) else {
        return Err("release API returned an invalid release".into());
    };
    if draft {
        return Ok(false);
    }
    if release.get("prerelease") != Some(&json!(false)) || release.get("immutable") != Some(&json!(true)) {
        return Err("current release must be stable and immutable".into());
    }
    Ok(true)
}

/// The status code of an `HTTP/VERSION CODE[ REASON]` first header line
/// (Python: `re.match(r'HTTP/\S+ (\d{3})(?: |$)', headers)`).
fn http_status(headers: &str) -> Option<&str> {
    let rest = headers.strip_prefix("HTTP/")?;
    let version_end = rest.find(char::is_whitespace).filter(|end| *end > 0)?;
    let code = rest[version_end..].strip_prefix(' ')?.get(..3)?;
    let after = &rest[version_end + 1 + 3..];
    let terminated = after.starts_with(' ') || after.is_empty() || after == "\n";
    (code.bytes().all(|byte| byte.is_ascii_digit()) && terminated).then_some(code)
}

/// `pr-ready`: reads `.release-please-manifest.json`, appends `ready=true`
/// or `ready=false` to `$GITHUB_OUTPUT`.
pub fn ready_command(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("usage: hamn-dev release pr-ready".into());
    }
    let manifest = super::files::read_json(Path::new(".release-please-manifest.json"))?;
    let repository = std::env::var("GITHUB_REPOSITORY").map_err(|_| "GITHUB_REPOSITORY is required")?;
    let output = std::env::var("GITHUB_OUTPUT").map_err(|_| "GITHUB_OUTPUT is required")?;
    let published = ready(&System, &repository, &manifest)?;
    OpenOptions::new()
        .append(true)
        .open(&output)
        .and_then(|mut file| writeln!(file, "ready={published}"))
        .map_err(|error| format!("{output}: {error}"))?;
    println!(
        "{}",
        if published {
            "Current release is published."
        } else {
            "Release publication is pending; defer release notes until Release succeeds."
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::http_status;

    #[test]
    fn status_line_grammar() {
        assert_eq!(http_status("HTTP/2.0 404 Not Found\nContent-Type: x"), Some("404"));
        assert_eq!(http_status("HTTP/1.1 200"), Some("200"));
        assert_eq!(http_status("HTTP/2.0 200\n"), Some("200"));
        for invalid in [
            "HTTP/2.0 200\nContent-Type: x",
            "HTTP/ 200 OK",
            "HTTP/2.0 20 OK",
            "HTTP/2.0 2000 OK",
            "http/2.0 200 OK",
            "HTTP/2.0  200 OK",
            "",
        ] {
            assert_eq!(http_status(invalid), None, "{invalid:?}");
        }
    }
}
