//! What the TUI says about a failure. A [`Failure`] carries a code and the
//! raw text of its source: the stderr of a Docker or kubectl CLI (with usage
//! text and client log lines), the Display of a Docker Engine or Kubernetes
//! API client error, or a sentence of Hamn's own. The screen shows a short
//! [`Summary`]: in one line what failed and, when known, what helps next.
//!
//! Contract: [`summarize`] is pure and total. Its title is never empty and
//! contains no control character, so it fits one line of the screen. It
//! never replaces the raw text: callers keep the `Failure` and show it
//! unchanged in the diagnostics view. A message is recognized only by a
//! wording that kubectl, the Docker CLI or their API clients print, each held
//! by a test that says whether it was recorded from a run; any other
//! message keeps its own first meaningful line, so an unknown cause is shown
//! in its own words and never replaced by a guess.

use crate::model::Failure;

/// How the screen marks a message: an error failed, a warning explains a
/// state the user can change, and info reports a result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    #[default]
    Info,
    Warning,
    Error,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    pub level: Level,
    pub title: String,
    /// The next step, naming keys of the TUI.
    pub hint: Option<&'static str>,
}

const KUBE_UNREACHABLE: &str = "Check that the cluster is running; e selects another context";
const DOCKER_UNREACHABLE: &str = "Check that the daemon is running; e selects another environment";
const OPTIONS: &str = "The command's --help lists its options";

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Docker,
    Kubernetes,
    Other,
}

pub fn summarize(failure: &Failure) -> Summary {
    let message = failure.message.as_str();
    match failure.code.as_str() {
        crate::tui_state::VM_NOT_READY | "staleData" | "noSelection" | "selectEnvironment" => {
            return summary(Level::Warning, sentence(message), None);
        }
        "queryTimeout" => {
            return summary(
                Level::Error,
                sentence(message),
                Some(":refresh-timeout <seconds> sets a longer deadline"),
            );
        }
        "cliUnavailable" => return cli_unavailable(message),
        "cliProtocol" => {
            return summary(
                Level::Error,
                "The CLI printed output that is not the expected JSON".into(),
                Some("! shows the output"),
            );
        }
        _ => {}
    }
    if let Some((program, status, stderr)) = cli_exit(message) {
        let source = if program == "docker" {
            Source::Docker
        } else {
            Source::Kubernetes
        };
        let lines = meaningful_lines(stderr);
        let text = lines.join("\n");
        if let Some((title, hint)) = recognize(source, &text) {
            return summary(Level::Error, title, hint);
        }
        let headline = lines.first().map_or("", |line| strip_prefixes(line));
        let title = if headline.is_empty() {
            // The exit status is then all that tells this failure from another.
            match status {
                "" => format!("{program} failed without an error message"),
                status => format!("{program} failed without an error message ({status})"),
            }
        } else if headline.starts_with(&format!("{program}:")) {
            single_line(headline)
        } else {
            format!("{program}: {}", single_line(headline))
        };
        let hint = (headline.contains("unknown flag")
            || headline.contains("unknown shorthand flag"))
        .then_some(OPTIONS);
        return summary(Level::Error, title, hint);
    }
    let (source, text) = client_text(&failure.code, message);
    if let Some((title, hint)) = recognize(source, &text) {
        return summary(Level::Error, title, hint);
    }
    let hint = match failure.code.as_str() {
        "outcomeUnknown" => {
            Some("The change may have been applied; inspect the target before retrying")
        }
        "kubernetesUnavailable" => Some(KUBE_UNREACHABLE),
        "dockerUnavailable" => Some(DOCKER_UNREACHABLE),
        _ => None,
    };
    let headline = meaningful_lines(&text)
        .first()
        .copied()
        .unwrap_or("")
        .to_owned();
    summary(Level::Error, sentence(strip_prefixes(&headline)), hint)
}

fn summary(level: Level, title: String, hint: Option<&'static str>) -> Summary {
    let title = single_line(&title);
    Summary {
        level,
        title: if title.is_empty() {
            "The operation failed without an error message".into()
        } else {
            title
        },
        hint,
    }
}

/// `native::query` reports an exited CLI as `<program> exited <status>:
/// <stderr>`, where the status reads `exit status: N` or `signal: N (NAME)`.
fn cli_exit(message: &str) -> Option<(&'static str, &str, &str)> {
    let (program, rest) = ["docker", "kubectl"].into_iter().find_map(|program| {
        Some((
            program,
            message.strip_prefix(program)?.strip_prefix(" exited ")?,
        ))
    })?;
    let (status, stderr) = if let Some(code) = rest.strip_prefix("exit status: ") {
        let end = "exit status: ".len() + code.bytes().take_while(u8::is_ascii_digit).count();
        (
            &rest[..end],
            rest[end..].strip_prefix(": ").unwrap_or(&rest[end..]),
        )
    } else {
        match rest.split_once("): ") {
            Some((status, stderr)) => (&rest[..status.len() + 1], stderr),
            None => ("", rest),
        }
    };
    Some((program, status, stderr))
}

/// `native::query` reports a CLI that could not be started as
/// `<program>: <error>`.
fn cli_unavailable(message: &str) -> Summary {
    let (program, error) = message.split_once(':').unwrap_or((message, ""));
    let name = match program {
        "docker" => "The Docker CLI",
        "kubectl" => "kubectl",
        _ => return summary(Level::Error, sentence(message), None),
    };
    if error.contains("No such file or directory") || error.contains("not found") {
        summary(
            Level::Error,
            format!("{name} is not installed or not on PATH"),
            Some("Install it, then press R"),
        )
    } else {
        summary(
            Level::Error,
            format!("{name} cannot be run: {}", error.trim()),
            None,
        )
    }
}

/// The text of an API client error without the client's framing: kube's
/// `ApiError: <message>: <reason> (Status {..})` and variant prefixes, and
/// bollard's `Docker responded with status code N: <message>`.
fn client_text(code: &str, message: &str) -> (Source, String) {
    if let Some(rest) = message.strip_prefix("ApiError: ") {
        let rest = rest.split(" (Status {").next().unwrap_or(rest);
        let rest = rest.split(" (ErrorResponse {").next().unwrap_or(rest);
        return (Source::Kubernetes, rest.to_owned());
    }
    if let Some(rest) = message.strip_prefix("Docker responded with status code ") {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let rest = rest[digits..].strip_prefix(": ").unwrap_or(&rest[digits..]);
        return (Source::Docker, rest.to_owned());
    }
    let source = match code {
        "kubernetesUnavailable" | "kubernetesError" => Source::Kubernetes,
        "dockerUnavailable" | "engineError" => Source::Docker,
        _ => Source::Other,
    };
    let mut text = message;
    for prefix in [
        "HyperError: ",
        "ServiceError: ",
        "HttpError: ",
        "auth error: ",
    ] {
        text = text.strip_prefix(prefix).unwrap_or(text);
    }
    (source, text.to_owned())
}

fn recognize(source: Source, text: &str) -> Option<(String, Option<&'static str>)> {
    if let Some(rest) = after(text, "The connection to the server ") {
        let host = rest.split(" was refused").next().unwrap_or(rest);
        return Some((
            format!("Cannot reach the Kubernetes API server at {host}: connection refused"),
            Some(KUBE_UNREACHABLE),
        ));
    }
    if let Some(cause) = after(text, "Unable to connect to the server: ") {
        return Some(kube_connection(cause.lines().next().unwrap_or(cause)));
    }
    if text.contains("You must be logged in to the server")
        || text.contains("the server has asked for the client to provide credentials")
    {
        return Some((
            "Kubernetes rejected the credentials of this context".into(),
            Some("Sign in again for this context, then press R"),
        ));
    }
    if let Some(rest) = after(text, "is forbidden: ") {
        let rest = rest.lines().next().unwrap_or(rest);
        let rest = rest
            .trim_end_matches(": Forbidden")
            .replace(" in API group \"\"", "");
        return Some((
            format!("Permission denied: {rest}"),
            Some("n selects another namespace; e another context"),
        ));
    }
    // A partial discovery: kubectl names each group that it could not list
    // and why, after it listed the others.
    if let Some(rest) = after(
        text,
        "unable to retrieve the complete list of server APIs: ",
    ) {
        return Some((
            format!(
                "These API groups could not be listed: {}",
                rest.lines().next().unwrap_or(rest)
            ),
            Some(":get apiservices shows whether each group's service is available"),
        ));
    }
    if let Some(rest) = after(text, "the server doesn't have a resource type \"") {
        let kind = rest.split('"').next().unwrap_or(rest);
        return Some((
            format!("This cluster has no resource type \"{kind}\""),
            Some(":api-resources lists the types it serves"),
        ));
    }
    if let Some(rest) = after(text, "context was not found for specified context: ") {
        return Some((
            format!(
                "Context {} is not in the kubeconfig",
                rest.lines().next().unwrap_or(rest).trim()
            ),
            Some("e selects a context"),
        ));
    }
    if let Some(rest) = after(text, "unable to resolve docker endpoint: context \"") {
        let name = rest.split('"').next().unwrap_or(rest);
        return Some((
            format!("Docker context {name} does not exist"),
            Some("e selects an environment"),
        ));
    }
    if let Some(rest) = after(
        text,
        "permission denied while trying to connect to the docker API at ",
    )
    .or_else(|| {
        after(
            text,
            "permission denied while trying to connect to the Docker daemon socket at ",
        )
    }) {
        let endpoint = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches([';', ':', '.']);
        return Some((
            format!("Permission denied on the Docker socket {endpoint}")
                .trim_end()
                .to_owned(),
            Some("Check the socket's owner and your group membership"),
        ));
    }
    if source != Source::Kubernetes {
        if let Some(rest) = after(text, "failed to connect to the docker API at ")
            .or_else(|| after(text, "Cannot connect to the Docker daemon at "))
        {
            let endpoint = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches([';', '.']);
            return Some((
                format!("Cannot connect to the Docker daemon at {endpoint}"),
                Some(DOCKER_UNREACHABLE),
            ));
        }
        if text.contains("error during connect")
            || text.contains("the docker daemon is not running")
        {
            return Some((
                "Cannot connect to the Docker daemon".into(),
                Some(DOCKER_UNREACHABLE),
            ));
        }
        if let Some(rest) = after(text, "Error response from daemon: ") {
            return Some((sentence(rest.lines().next().unwrap_or(rest)), None));
        }
    }
    if let Some(rest) = after(text, "Error from server (NotFound): ") {
        let rest = rest.lines().next().unwrap_or(rest);
        let hint = rest
            .starts_with("namespaces \"")
            .then_some("n selects a namespace");
        return Some((sentence(rest), hint));
    }
    if let Some(rest) = after(text, "Error from server (") {
        let rest = rest.lines().next().unwrap_or(rest);
        let rest = rest.split_once("): ").map_or(rest, |(_, message)| message);
        return Some((sentence(rest), None));
    }
    if source == Source::Kubernetes
        && [
            "Connection refused",
            "connection refused",
            "tcp connect error",
            "client error (Connect)",
        ]
        .iter()
        .any(|cause| text.contains(cause))
    {
        return Some((
            "Cannot reach the Kubernetes API server".into(),
            Some(KUBE_UNREACHABLE),
        ));
    }
    None
}

fn kube_connection(cause: &str) -> (String, Option<&'static str>) {
    if let Some(rest) = after(cause, "lookup ") {
        let host = rest.split([':', ' ']).next().unwrap_or(rest);
        if cause.contains("no such host") {
            return (
                format!("Cannot resolve the Kubernetes API server host {host}"),
                Some(KUBE_UNREACHABLE),
            );
        }
    }
    if [
        "i/o timeout",
        "context deadline exceeded",
        "TLS handshake timeout",
        "Client.Timeout",
    ]
    .iter()
    .any(|timeout| cause.contains(timeout))
    {
        return (
            "The Kubernetes API server did not answer in time".into(),
            Some(KUBE_UNREACHABLE),
        );
    }
    if cause.contains("x509:") || cause.contains("failed to verify certificate") {
        return (
            "The certificate of the Kubernetes API server is not trusted".into(),
            Some("Check the cluster's certificate authority in the kubeconfig"),
        );
    }
    if cause.contains("connection refused") {
        let host = after(cause, "dial tcp ")
            .and_then(|rest| rest.split(": ").next())
            .unwrap_or("");
        let at = if host.is_empty() {
            String::new()
        } else {
            format!(" at {host}")
        };
        return (
            format!("Cannot reach the Kubernetes API server{at}: connection refused"),
            Some(KUBE_UNREACHABLE),
        );
    }
    (
        format!("Cannot reach the Kubernetes API server: {cause}"),
        Some(KUBE_UNREACHABLE),
    )
}

fn after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text.find(marker).map(|start| &text[start + marker.len()..])
}

/// The lines of CLI stderr that describe the failure: without kubectl's
/// client log lines (`E1015 01:17:26.408480 67207 memcache.go:381] ...`),
/// help pointers and the usage text that Docker prints after an error.
/// Client log lines are kept only when nothing else explains the failure.
fn meaningful_lines(stderr: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut logs = Vec::new();
    for line in stderr.lines().map(str::trim) {
        if line.starts_with("Usage:") {
            break;
        }
        if line.is_empty()
            || (line.starts_with("Run '") && line.contains("--help"))
            || (line.starts_with("See '") && line.contains("help"))
            || (line.starts_with("Use \"") && line.contains("--help"))
        {
            continue;
        }
        if client_log(line) {
            logs.push(line);
        } else {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        if let Some(log) = logs.last() {
            let text = log.split_once("] ").map_or(*log, |(_, text)| text);
            lines.push(after(text, "err=\"").map_or(text, |err| err.trim_end_matches('"')));
        }
    }
    lines
}

fn client_log(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() > 6
        && matches!(bytes[0], b'I' | b'W' | b'E' | b'F')
        && bytes[1..5].iter().all(u8::is_ascii_digit)
        && bytes[5] == b' '
        && line.contains("] ")
}

fn strip_prefixes(line: &str) -> &str {
    let mut line = line.trim();
    for prefix in [
        "error: ",
        "Error: ",
        "Failed to initialize: ",
        "Error in configuration: ",
    ] {
        line = line.strip_prefix(prefix).unwrap_or(line);
    }
    line
}

fn single_line(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .replace("\\\"", "\"")
}

/// A one-line sentence that starts with a capital letter.
fn sentence(text: &str) -> String {
    let text = single_line(text);
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The message `native::query` makes of an exited CLI. Its code, cliError
    // or dockerUnreachable, does not change what the screen says.
    fn cli(program: &str, status: &str, stderr: &str) -> Failure {
        Failure::new("cliError", format!("{program} exited {status}: {stderr}"))
    }

    // Recorded from kubectl 1.37.1 against a kind cluster whose container is
    // stopped: client log lines first, then the sentence that explains them.
    const KUBECTL_REFUSED: &str = "E1015 01:17:26.408480   67207 memcache.go:381] \"Couldn't get current server API group list\" err=\"Get \\\"https://127.0.0.1:63621/api?timeout=32s\\\": dial tcp 127.0.0.1:63621: connect: connection refused\"\nE1015 01:17:26.408831   67207 memcache.go:381] \"Couldn't get current server API group list\" err=\"Get \\\"https://127.0.0.1:63621/api?timeout=32s\\\": dial tcp 127.0.0.1:63621: connect: connection refused\"\nThe connection to the server 127.0.0.1:63621 was refused - did you specify the right host or port?\n";

    #[test]
    fn recorded_cli_errors_become_one_line_with_the_key_that_helps() {
        for (failure, title, hint) in [
            (
                cli("kubectl", "exit status: 1", KUBECTL_REFUSED),
                "Cannot reach the Kubernetes API server at 127.0.0.1:63621: connection refused",
                Some(KUBE_UNREACHABLE),
            ),
            // Recorded from kubectl 1.37.1 with --context nope.
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "Error in configuration: context was not found for specified context: nope\n",
                ),
                "Context nope is not in the kubeconfig",
                Some("e selects a context"),
            ),
            // Recorded from kubectl 1.37.1 running `api-resources -o name`
            // against the tui-command-completion suite's fake API, whose
            // group broken.fixture.hamn.dev/v1 answers 503: client log lines,
            // then the groups it could not list. It exited 1 after printing
            // the names of the other groups.
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    concat!(
                        "E1015 16:03:48.475341   15898 memcache.go:403] \"Couldn't get resource list\" err=\"the server is currently unable to handle the request\" groupVersion=\"broken.fixture.hamn.dev/v1\"\n",
                        "E1015 16:03:48.525025   15898 memcache.go:133] \"Couldn't get resource list\" err=\"the server is currently unable to handle the request\" gv=\"broken.fixture.hamn.dev/v1\"\n",
                        "error: unable to retrieve the complete list of server APIs: broken.fixture.hamn.dev/v1: the server is currently unable to handle the request\n",
                    ),
                ),
                "These API groups could not be listed: broken.fixture.hamn.dev/v1: the server is currently unable to handle the request",
                Some(":get apiservices shows whether each group's service is available"),
            ),
            // kubectl's wordings, not recorded here: an API refusal, a missing
            // object or type, rejected credentials, a server it cannot resolve,
            // reach in time or trust, and a usage error.
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "Error from server (Forbidden): pods is forbidden: User \"system:serviceaccount:default:viewer\" cannot list resource \"pods\" in API group \"\" in the namespace \"kube-system\"\n",
                ),
                "Permission denied: User \"system:serviceaccount:default:viewer\" cannot list resource \"pods\" in the namespace \"kube-system\"",
                Some("n selects another namespace; e another context"),
            ),
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "Error from server (NotFound): namespaces \"missing\" not found\n",
                ),
                "Namespaces \"missing\" not found",
                Some("n selects a namespace"),
            ),
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "Error from server (NotFound): pods \"api\" not found\n",
                ),
                "Pods \"api\" not found",
                None,
            ),
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "error: the server doesn't have a resource type \"widgets\"\n",
                ),
                "This cluster has no resource type \"widgets\"",
                Some(":api-resources lists the types it serves"),
            ),
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "error: You must be logged in to the server (Unauthorized)\n",
                ),
                "Kubernetes rejected the credentials of this context",
                Some("Sign in again for this context, then press R"),
            ),
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "Unable to connect to the server: dial tcp: lookup api.example.invalid on 1.1.1.1:53: no such host\n",
                ),
                "Cannot resolve the Kubernetes API server host api.example.invalid",
                Some(KUBE_UNREACHABLE),
            ),
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "Unable to connect to the server: dial tcp 10.0.0.1:6443: i/o timeout\n",
                ),
                "The Kubernetes API server did not answer in time",
                Some(KUBE_UNREACHABLE),
            ),
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "Unable to connect to the server: tls: failed to verify certificate: x509: certificate signed by unknown authority\n",
                ),
                "The certificate of the Kubernetes API server is not trusted",
                Some("Check the cluster's certificate authority in the kubeconfig"),
            ),
            (
                cli(
                    "kubectl",
                    "exit status: 1",
                    "error: unknown flag: --bogus\nSee 'kubectl get --help' for usage.\n",
                ),
                "kubectl: unknown flag: --bogus",
                Some(OPTIONS),
            ),
            // Recorded from Docker CLI 29.9.0: a missing socket, a missing
            // context and a usage error. The other connection messages are those
            // that native.rs records from 29.8.1 and an earlier release; the
            // socket permission and daemon answers are Docker's wordings.
            (
                cli(
                    "docker",
                    "exit status: 1",
                    "failed to connect to the docker API at unix:///Users/me/.hamn/default/docker.sock; check if the path is correct and if the daemon is running: dial unix /Users/me/.hamn/default/docker.sock: connect: no such file or directory\n",
                ),
                "Cannot connect to the Docker daemon at unix:///Users/me/.hamn/default/docker.sock",
                Some(DOCKER_UNREACHABLE),
            ),
            (
                cli(
                    "docker",
                    "exit status: 1",
                    "error during connect: Get \"http://%2Fhome%2F.hamn%2Fdefault%2Fdocker.sock/v1.56/containers/json\": EOF\n",
                ),
                "Cannot connect to the Docker daemon",
                Some(DOCKER_UNREACHABLE),
            ),
            (
                cli(
                    "docker",
                    "exit status: 1",
                    "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?\n",
                ),
                "Cannot connect to the Docker daemon at unix:///var/run/docker.sock",
                Some(DOCKER_UNREACHABLE),
            ),
            (
                cli(
                    "docker",
                    "exit status: 1",
                    "Failed to initialize: unable to resolve docker endpoint: context \"nope\": context not found: open /Users/me/.docker/contexts/meta/ca37/meta.json: no such file or directory\n",
                ),
                "Docker context nope does not exist",
                Some("e selects an environment"),
            ),
            (
                cli(
                    "docker",
                    "exit status: 1",
                    "permission denied while trying to connect to the docker API at unix:///var/run/docker.sock\n",
                ),
                "Permission denied on the Docker socket unix:///var/run/docker.sock",
                Some("Check the socket's owner and your group membership"),
            ),
            (
                cli(
                    "docker",
                    "exit status: 125",
                    "unknown flag: --bogus-flag\n\nUsage:  docker ps [OPTIONS]\n\nRun 'docker ps --help' for more information\n",
                ),
                "docker: unknown flag: --bogus-flag",
                Some(OPTIONS),
            ),
            (
                cli(
                    "docker",
                    "exit status: 1",
                    "docker: unknown command: docker compose\n\nRun 'docker --help' for more information\n",
                ),
                "docker: unknown command: docker compose",
                None,
            ),
            (
                cli(
                    "docker",
                    "exit status: 1",
                    "Error response from daemon: No such container: web\n",
                ),
                "No such container: web",
                None,
            ),
            (
                cli("docker", "signal: 9 (SIGKILL)", ""),
                "docker failed without an error message (signal: 9 (SIGKILL))",
                None,
            ),
            (
                cli("docker", "exit status: 7", ""),
                "docker failed without an error message (exit status: 7)",
                None,
            ),
        ] {
            let summary = summarize(&failure);
            assert_eq!(
                (summary.title.as_str(), summary.hint),
                (title, hint),
                "{}",
                failure.message
            );
            assert_eq!(summary.level, Level::Error);
        }
    }

    #[test]
    fn client_errors_lose_their_framing_but_not_their_cause() {
        let forbidden = Failure::new(
            "permissionDenied",
            "ApiError: namespaces is forbidden: User \"dev\" cannot list resource \"namespaces\" in API group \"\" at the cluster scope: Forbidden (Status { status: Some(\"Failure\"), code: 403 })",
        );
        assert_eq!(
            summarize(&forbidden).title,
            "Permission denied: User \"dev\" cannot list resource \"namespaces\" at the cluster scope"
        );
        let refused = Failure::new(
            "kubernetesUnavailable",
            "HyperError: client error (Connect): tcp connect error: Connection refused (os error 61)",
        );
        assert_eq!(
            (summarize(&refused).title.as_str(), summarize(&refused).hint),
            (
                "Cannot reach the Kubernetes API server",
                Some(KUBE_UNREACHABLE)
            )
        );
        let missing = Failure::new(
            "notFound",
            "Docker responded with status code 404: No such container: web",
        );
        assert_eq!(summarize(&missing).title, "No such container: web");
        // An unrecognized client error keeps its own words and the source's hint.
        let other = Failure::new(
            "dockerUnavailable",
            "Socket not found: /var/run/docker.sock",
        );
        assert_eq!(
            (summarize(&other).title.as_str(), summarize(&other).hint),
            (
                "Socket not found: /var/run/docker.sock",
                Some(DOCKER_UNREACHABLE)
            )
        );
    }

    #[test]
    fn hamn_failures_keep_their_sentence_and_state_guidance_is_a_warning() {
        let guidance = "Hamn VM default is not running (stopped). Press s to start it; the list appears when Docker is ready.";
        let summary = summarize(&Failure::new(crate::tui_state::VM_NOT_READY, guidance));
        assert_eq!(
            (summary.level, summary.title.as_str(), summary.hint),
            (Level::Warning, guidance, None)
        );
        let stale = summarize(&Failure::new(
            "staleData",
            "refresh before acting on previous data",
        ));
        assert_eq!(
            (stale.level, stale.title.as_str()),
            (Level::Warning, "Refresh before acting on previous data")
        );
        let timeout = summarize(&Failure::new(
            "queryTimeout",
            "Query exceeded 1 seconds; owned CLI helpers stopped",
        ));
        assert_eq!(
            timeout.title,
            "Query exceeded 1 seconds; owned CLI helpers stopped"
        );
        assert!(timeout.hint.unwrap().contains(":refresh-timeout"));
        let overflow = summarize(&Failure::new(
            "cliError",
            "CLI output exceeds 16 MiB; / filters fetched rows only. Narrow with --namespace, --selector or --field-selector.",
        ));
        assert!(
            overflow.title.starts_with("CLI output exceeds 16 MiB"),
            "{}",
            overflow.title
        );
        for (message, title) in [
            (
                "docker: No such file or directory (os error 2)",
                "The Docker CLI is not installed or not on PATH",
            ),
            (
                "kubectl: No such file or directory (os error 2)",
                "kubectl is not installed or not on PATH",
            ),
            (
                "docker: Permission denied (os error 13)",
                "The Docker CLI cannot be run: Permission denied (os error 13)",
            ),
        ] {
            assert_eq!(
                summarize(&Failure::new("cliUnavailable", message)).title,
                title
            );
        }
        let unknown = summarize(&Failure::new(
            "outcomeUnknown",
            "request accepted\nresult lost",
        ));
        assert_eq!(unknown.title, "Request accepted");
        assert!(unknown.hint.unwrap().contains("inspect the target"));
    }

    #[test]
    fn titles_are_one_nonempty_line_for_any_input() {
        for message in [
            "",
            "\n\n",
            "docker exited exit status: 1: ",
            "kubectl exited exit status: 1: \n\nUsage:\n  kubectl get",
            "kubectl exited exit status: 1: E1015 01:17:26.408480   67207 memcache.go:381] \"x\" err=\"dial tcp: connect: connection refused\"",
            "docker exited exit status: 1: \u{1b}[31mred\u{1b}[0m\r\nline\ttab",
            "kubectl exited signal: 6 (SIGABRT) (core dumped): panic",
            "긴 진단\n두 번째 줄",
            "ApiError: ",
            "Docker responded with status code ",
        ] {
            for code in [
                "cliError",
                "dockerUnreachable",
                "kubernetesError",
                "permissionDenied",
                "cliUnavailable",
                "other",
            ] {
                let summary = summarize(&Failure::new(code, message));
                assert!(!summary.title.is_empty(), "{code}: {message:?}");
                assert!(
                    !summary.title.chars().any(char::is_control),
                    "{code}: {message:?} -> {:?}",
                    summary.title
                );
            }
        }
        // A status that cannot be read is left out rather than shown empty.
        assert_eq!(
            summarize(&Failure::new("cliError", "docker exited ")).title,
            "docker failed without an error message"
        );
        // Client log lines alone still say what failed: the error they carry.
        let logs = "kubectl exited exit status: 1: E1015 01:17:26.408480   67207 memcache.go:381] \"x\" err=\"dial tcp: connect: connection refused\"";
        assert_eq!(
            summarize(&Failure::new("cliError", logs)).title,
            "Cannot reach the Kubernetes API server"
        );
        let logs = "kubectl exited exit status: 1: W1015 01:17:26.408480   67207 loader.go:223] \"x\" err=\"exec plugin: invalid apiVersion\"";
        assert_eq!(
            summarize(&Failure::new("cliError", logs)).title,
            "kubectl: exec plugin: invalid apiVersion"
        );
        assert_eq!(
            summarize(&Failure::new(
                "cliError",
                "kubectl exited signal: 6 (SIGABRT) (core dumped): panic"
            ))
            .title,
            "kubectl: panic"
        );
    }
}
