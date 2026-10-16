//! The kinds that a Kubernetes cluster serves, custom resources included,
//! for the `get <kind>` completion of the `:` prompt, as k9s completes
//! them: the output of `kubectl api-resources -o name` for the kubeconfig
//! and context that a typed command would use.
//!
//! Contract:
//! - `ClusterKinds` is the lookup state of one `State`. It holds no process
//!   and reads no clock. `tui.rs` starts a lookup when the `:` prompt opens
//!   in Kubernetes and `wanted` says so, runs `lookup` in a task, and hands
//!   its `Listing` to `finish` with the ticket that `begin` returned. At most
//!   one lookup runs at a time; a keystroke never waits for one.
//! - A lookup belongs to one `Target`. Its kinds are offered only while the
//!   state's kubeconfig and context are still that target. `retarget` drops
//!   a running lookup for another target, and a result whose ticket is not
//!   the running lookup's is discarded.
//! - A `Listing` holds the words a lookup found and, when it failed, why.
//!   kubectl exits unsuccessfully on a partial discovery (an aggregated API
//!   such as `metrics.k8s.io` that does not answer) after it printed the
//!   names of the other groups: those names are offered, and the failure is
//!   reported as well. A lookup that found no names offers nothing beyond
//!   the built-in kinds.
//! - `reason` says why a lookup failed in one line, which the border of the
//!   prompt shows after `consequence`; the whole `failure`, kubectl's stderr
//!   included, is kept for the diagnostics view (`!`). Neither is the message
//!   under the view, which belongs to what the user ran.
//! - The next prompt retries a lookup that failed, wholly or partly. The
//!   last listing stays offered and shown while the retry runs, and the
//!   retry's listing replaces it, whatever it found.
//! - `lookup` runs kubectl as an owned `QueryProcess`, bounded by
//!   `deadline`: when the deadline passes, or the future is dropped because
//!   the target changed or the TUI quits, the process group is killed and
//!   its leader reaped. It reads at most `OUTPUT_LIMIT` bytes of each stream.
//!   Its failures are `cliUnavailable` (kubectl did not start), `cliError`
//!   (it exited unsuccessfully, reported as `native::query` reports one),
//!   `cliOutput` (its output could not be read or was too long),
//!   `queryTimeout` and `cliProtocol` (`words` refused the output of a
//!   successful exit).
//! - `words` turns the output into the words that `get` completes.

use crate::model::{Failure, Request, Result};
use crate::query_process::QueryProcess;
use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitStatus;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// The most a lookup reads of stdout or stderr. A cluster with a thousand
/// custom resources prints about 50 KiB of names.
const OUTPUT_LIMIT: u64 = 1024 * 1024;

/// The kubeconfig and context of a lookup: those that a typed command uses
/// unless it names others. `None` leaves the choice to kubectl, as
/// `native::parse` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub kubeconfig: Option<String>,
    pub context: Option<String>,
}

impl Target {
    pub fn of(request: &Request) -> Self {
        Self {
            kubeconfig: request.kubeconfig.clone(),
            context: request.context.clone(),
        }
    }
}

/// What a lookup found: the words that `get` completes, and why the list
/// may lack kinds. A complete listing has no failure, a failed one no
/// words, and a partial discovery both.
#[derive(Debug, Default)]
pub struct Listing {
    pub words: Vec<String>,
    pub failure: Option<Failure>,
}

impl Listing {
    pub fn complete(words: Vec<String>) -> Self {
        Self {
            words,
            failure: None,
        }
    }

    pub fn failed(failure: Failure) -> Self {
        Self {
            words: Vec::new(),
            failure: Some(failure),
        }
    }
}

#[derive(Debug, Default)]
enum Status {
    #[default]
    Idle,
    /// `previous` is the last listing of the same target, which failed and
    /// stays offered and shown while this lookup retries it.
    Running {
        ticket: u64,
        previous: Option<Listing>,
    },
    Finished(Listing),
}

#[derive(Debug, Default)]
pub struct ClusterKinds {
    /// What `status` is about; `None` while `Idle`.
    target: Option<Target>,
    status: Status,
    /// The last ticket `begin` returned.
    tickets: u64,
}

impl ClusterKinds {
    /// Whether opening the prompt for `target` starts a lookup: none has
    /// run for it, or the last one failed, wholly or partly. A running
    /// lookup is waited for, and a complete listing is kept.
    pub fn wanted(&self, target: &Target) -> bool {
        self.target.as_ref() != Some(target)
            || match &self.status {
                Status::Idle => true,
                Status::Running { .. } => false,
                Status::Finished(listing) => listing.failure.is_some(),
            }
    }

    /// Records that a lookup of `target` starts, in place of anything known
    /// before, and returns its ticket for `finish`.
    pub fn begin(&mut self, target: Target) -> u64 {
        let same = self.target.as_ref() == Some(&target);
        let previous = match std::mem::take(&mut self.status) {
            Status::Finished(listing) if same && listing.failure.is_some() => Some(listing),
            Status::Running { previous, .. } if same => previous,
            _ => None,
        };
        self.tickets += 1;
        self.target = Some(target);
        self.status = Status::Running {
            ticket: self.tickets,
            previous,
        };
        self.tickets
    }

    /// Takes the listing of the lookup with `ticket` and returns whether it
    /// was the running lookup. The listing of any other lookup is stale and
    /// changes nothing.
    pub fn finish(&mut self, ticket: u64, listing: Listing) -> bool {
        if !matches!(self.status, Status::Running { ticket: running, .. } if running == ticket) {
            return false;
        }
        self.status = Status::Finished(listing);
        true
    }

    /// Forgets what is known about any target other than `target`. Returns
    /// whether that dropped a running lookup, whose task the caller stops.
    pub fn retarget(&mut self, target: &Target) -> bool {
        if self.target.as_ref() == Some(target) {
            return false;
        }
        self.target = None;
        matches!(std::mem::take(&mut self.status), Status::Running { .. })
    }

    /// The last listing of `target`: the finished one, or while a retry
    /// runs, the one it retries.
    fn listing(&self, target: &Target) -> Option<&Listing> {
        if self.target.as_ref() != Some(target) {
            return None;
        }
        match &self.status {
            Status::Finished(listing)
            | Status::Running {
                previous: Some(listing),
                ..
            } => Some(listing),
            _ => None,
        }
    }

    /// The words, beyond the built-in kinds, that `get` completes for
    /// `target`: those its last listing found.
    pub fn offered(&self, target: &Target) -> &[String] {
        self.listing(target).map_or(&[], |listing| &listing.words)
    }

    /// Why the last lookup of `target` failed, wholly or partly.
    pub fn failure(&self, target: &Target) -> Option<&Failure> {
        self.listing(target)?.failure.as_ref()
    }

    /// What a failed lookup of `target` leaves `get` to complete, as the
    /// border of the prompt and the diagnostics view say it.
    pub fn consequence(&self, target: &Target) -> &'static str {
        if self.offered(target).is_empty() {
            "get completes built-in kinds only"
        } else {
            "get completes only the kinds kubectl listed"
        }
    }
}

/// The failure of a lookup in one line for the border of the prompt. What
/// kubectl said is summarized as the message line summarizes a CLI's
/// failure (`tui_failure`), which skips its client log lines and names the
/// causes it knows. Hamn's own reasons are one sentence already.
pub fn reason(failure: &Failure) -> String {
    match failure.code.as_str() {
        "cliError" | "cliUnavailable" => crate::tui_failure::summarize(failure).title,
        _ => failure.message.clone(),
    }
}

/// Runs `kubectl [--kubeconfig K] [--context C] api-resources -o name`
/// for `target`; see the module contract.
pub async fn lookup(target: &Target, deadline: Duration) -> Listing {
    listing(command(target), deadline).await
}

fn command(target: &Target) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("kubectl");
    if let Some(config) = &target.kubeconfig {
        command.args(["--kubeconfig", config]);
    }
    if let Some(context) = &target.context {
        command.args(["--context", context]);
    }
    command.args(["api-resources", "-o", "name"]);
    command
}

/// What `command` lists before `deadline`. After a successful exit, stdout
/// must be a list of names. After an unsuccessful one, stdout that is a
/// list of names is still offered, as kubectl's partial discovery prints
/// one; stdout that is not is not offered, and the failure reported is the
/// exit's, since kubectl's stderr says more than its stray output.
async fn listing(command: tokio::process::Command, deadline: Duration) -> Listing {
    let (status, stdout, stderr) = match output(command, deadline).await {
        Ok(output) => output,
        Err(failure) => return Listing::failed(failure),
    };
    let words = words(&stdout);
    if status.success() {
        return match words {
            Ok(words) => Listing::complete(words),
            Err(failure) => Listing::failed(failure),
        };
    }
    Listing {
        words: words.unwrap_or_default(),
        failure: Some(Failure::new(
            "cliError",
            format!(
                "kubectl exited {status}: {}",
                String::from_utf8_lossy(&stderr)
            ),
        )),
    }
}

/// The exit status, stdout and stderr of `command`, which must exit before
/// `deadline`.
async fn output(
    command: tokio::process::Command,
    deadline: Duration,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    async fn read(stream: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stream
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() as u64 > OUTPUT_LIMIT {
            return Err(std::io::Error::other(format!(
                "output exceeds {} KiB",
                OUTPUT_LIMIT / 1024
            )));
        }
        Ok(bytes)
    }
    let run = async {
        // Dropping `process` before it is reaped, on an error here or when
        // the deadline drops this future, kills its group and reaps it.
        let (mut process, stdout, stderr) = QueryProcess::spawn(command)
            .map_err(|error| Failure::new("cliUnavailable", format!("kubectl: {error}")))?;
        tokio::try_join!(process.wait(), read(stdout), read(stderr))
            .map_err(|error| Failure::new("cliOutput", format!("kubectl api-resources: {error}")))
    };
    tokio::time::timeout(deadline, run)
        .await
        .unwrap_or_else(|_| {
            Err(Failure::new(
                "queryTimeout",
                format!("kubectl api-resources did not finish in {deadline:?} and was stopped"),
            ))
        })
}

fn shorten(text: &str) -> String {
    const LIMIT: usize = 120;
    if text.chars().count() <= LIMIT {
        return text.into();
    }
    text.chars().take(LIMIT).chain(['…']).collect()
}

/// The words that `get` completes, from the output of
/// `kubectl api-resources -o name`: one resource per line, its plural for
/// the core group and `plural.group` for the others.
///
/// A plural that one group serves is offered as itself, which kubectl
/// resolves. A plural that several groups serve (`pods` and
/// `pods.metrics.k8s.io`) is offered as each of its lines, the core one as
/// its plural, so that a completion never stands for a group the user does
/// not see. Repeated lines count once. The words are sorted shortest
/// first, then by their bytes, so that a typed prefix completes to the
/// shortest kind that extends it (`cert` to `certificates`, not to
/// `certificaterequests`). Empty output has no words. A line that is not a
/// resource name, or output that does not end with a newline, is not what
/// `-o name` prints, and the whole output is refused.
pub fn words(output: &[u8]) -> Result<Vec<String>> {
    let refuse = |what: String| {
        Failure::new(
            "cliProtocol",
            format!("kubectl api-resources printed {what}"),
        )
    };
    let text =
        std::str::from_utf8(output).map_err(|_| refuse("output that is not UTF-8".into()))?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let body = text
        .strip_suffix('\n')
        .ok_or_else(|| refuse("a last line without a newline".into()))?;
    let names = body
        .split('\n')
        .map(|line| {
            if resource_name(line) {
                Ok(line)
            } else {
                Err(refuse(format!(
                    "{:?}, which is not a resource name",
                    shorten(line)
                )))
            }
        })
        .collect::<Result<BTreeSet<&str>>>()?;
    let mut groups: BTreeMap<&str, usize> = BTreeMap::new();
    for name in &names {
        *groups.entry(plural_of(name)).or_default() += 1;
    }
    let words: BTreeSet<&str> = names
        .iter()
        .map(|name| {
            let plural = plural_of(name);
            if groups[plural] == 1 { plural } else { name }
        })
        .collect();
    let mut words: Vec<String> = words.into_iter().map(String::from).collect();
    words.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    Ok(words)
}

fn plural_of(name: &str) -> &str {
    name.split_once('.').map_or(name, |(plural, _)| plural)
}

/// A resource name as `-o name` prints it: a plural that is a DNS label
/// and, outside the core group, `.` and a group that is a DNS subdomain.
/// Every such name is also a word that `native::parse` opens as a view.
fn resource_name(line: &str) -> bool {
    match line.split_once('.') {
        None => dns_label(line),
        Some((plural, group)) => {
            dns_label(plural) && group.len() <= 253 && group.split('.').all(dns_label)
        }
    }
}

/// A DNS label of RFC 1123: 1 to 63 lowercase letters, digits or `-`, with
/// a letter or digit at each end.
fn dns_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

/// What `kubectl api-resources -o name` printed for a kind cluster with
/// cert-manager and metrics-server: the core group without a group name,
/// `events` and `pods` and `nodes` served by two groups each.
#[cfg(test)]
pub(crate) const SAMPLE_OUTPUT: &str = "bindings
componentstatuses
configmaps
endpoints
events
limitranges
namespaces
nodes
persistentvolumeclaims
persistentvolumes
pods
podtemplates
replicationcontrollers
resourcequotas
secrets
serviceaccounts
services
challenges.acme.cert-manager.io
orders.acme.cert-manager.io
mutatingwebhookconfigurations.admissionregistration.k8s.io
validatingwebhookconfigurations.admissionregistration.k8s.io
customresourcedefinitions.apiextensions.k8s.io
apiservices.apiregistration.k8s.io
controllerrevisions.apps
daemonsets.apps
deployments.apps
replicasets.apps
statefulsets.apps
selfsubjectreviews.authentication.k8s.io
tokenreviews.authentication.k8s.io
localsubjectaccessreviews.authorization.k8s.io
selfsubjectaccessreviews.authorization.k8s.io
selfsubjectrulesreviews.authorization.k8s.io
subjectaccessreviews.authorization.k8s.io
horizontalpodautoscalers.autoscaling
cronjobs.batch
jobs.batch
certificaterequests.cert-manager.io
certificates.cert-manager.io
clusterissuers.cert-manager.io
issuers.cert-manager.io
certificatesigningrequests.certificates.k8s.io
leases.coordination.k8s.io
endpointslices.discovery.k8s.io
events.events.k8s.io
flowschemas.flowcontrol.apiserver.k8s.io
prioritylevelconfigurations.flowcontrol.apiserver.k8s.io
nodes.metrics.k8s.io
pods.metrics.k8s.io
ingressclasses.networking.k8s.io
ingresses.networking.k8s.io
networkpolicies.networking.k8s.io
runtimeclasses.node.k8s.io
poddisruptionbudgets.policy
clusterrolebindings.rbac.authorization.k8s.io
clusterroles.rbac.authorization.k8s.io
rolebindings.rbac.authorization.k8s.io
roles.rbac.authorization.k8s.io
priorityclasses.scheduling.k8s.io
csidrivers.storage.k8s.io
csinodes.storage.k8s.io
csistoragecapacities.storage.k8s.io
storageclasses.storage.k8s.io
volumeattachments.storage.k8s.io
";

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    fn target(context: &str) -> Target {
        Target {
            kubeconfig: None,
            context: Some(context.into()),
        }
    }

    #[test]
    fn a_plural_is_offered_alone_unless_several_groups_serve_it() {
        let offered_words = words(SAMPLE_OUTPUT.as_bytes()).unwrap();
        let lines = SAMPLE_OUTPUT.lines().count();
        // Every distinct line gives one word, so nothing is lost or merged.
        assert_eq!(offered_words.len(), lines, "{offered_words:?}");
        // Shortest first, then by bytes, without repeats.
        assert!(
            offered_words
                .windows(2)
                .all(|pair| (pair[0].len(), &pair[0]) < (pair[1].len(), &pair[1])),
            "{offered_words:?}"
        );
        assert_eq!(&offered_words[..3], ["jobs", "pods", "nodes"]);
        for offered in [
            // The core group, and plurals that one group serves.
            "configmaps",
            "deployments",
            "cronjobs",
            "certificates",
            "issuers",
            "challenges",
            // Served by two groups: the core one by its plural, the other by
            // its whole name.
            "events",
            "events.events.k8s.io",
            "pods",
            "pods.metrics.k8s.io",
            "nodes",
            "nodes.metrics.k8s.io",
        ] {
            assert!(
                offered_words.iter().any(|word| word == offered),
                "{offered}: {offered_words:?}"
            );
        }
        for hidden in [
            "deployments.apps",
            "certificates.cert-manager.io",
            "metrics.k8s.io",
        ] {
            assert!(
                !offered_words.iter().any(|word| word == hidden),
                "{hidden}: {offered_words:?}"
            );
        }
        // Two groups and no core one: neither may stand for the other.
        assert_eq!(
            words(b"certificates.cert-manager.io\ncertificates.networking.internal.knative.dev\nissuers.cert-manager.io\n")
                .unwrap(),
            strings(&[
                "issuers",
                "certificates.cert-manager.io",
                "certificates.networking.internal.knative.dev",
            ])
        );
    }

    #[test]
    fn repeated_lines_count_once_and_empty_output_has_no_words() {
        assert_eq!(words(b"").unwrap(), Vec::<String>::new());
        assert_eq!(
            words(b"pods\ncertificates.cert-manager.io\npods\ncertificates.cert-manager.io\n")
                .unwrap(),
            strings(&["pods", "certificates"])
        );
        // A repeated line is not a second group: the plural stays alone.
        assert_eq!(
            words(b"issuers.cert-manager.io\nissuers.cert-manager.io\n").unwrap(),
            strings(&["issuers"])
        );
    }

    #[test]
    fn output_that_is_not_a_list_of_names_is_refused_whole() {
        let label = "a".repeat(63);
        let group = format!("{label}.{label}.{label}.{}", "b".repeat(61));
        assert_eq!(group.len(), 253);
        // The longest plural and group that DNS names allow are accepted.
        assert_eq!(
            words(format!("{label}.{group}\n").as_bytes()).unwrap(),
            strings(&[&label])
        );
        for (output, why) in [
            (&b"\n"[..], "\"\""),
            (b"pods\n\nservices\n", "\"\""),
            (b"pods\nservices", "a last line without a newline"),
            (b"pods\r\n", "\"pods\\r\""),
            (b"Pods\n", "\"Pods\""),
            // What a generic fixture prints for an unknown command.
            (b"ACTION_DONE\n", "\"ACTION_DONE\""),
            (b"pods.\n", "\"pods.\""),
            (b".apps\n", "\".apps\""),
            (b"deployments..apps\n", "\"deployments..apps\""),
            (b"-pods\n", "\"-pods\""),
            (b"pods-\n", "\"pods-\""),
            (b"pods.-apps\n", "\"pods.-apps\""),
            (b"pod_templates\n", "\"pod_templates\""),
            (b"pods svc\n", "\"pods svc\""),
            (b"NAME\npods\n", "\"NAME\""),
            (b"pods\n\xff\n", "output that is not UTF-8"),
        ] {
            let failure = words(output).unwrap_err();
            assert_eq!(failure.code, "cliProtocol", "{output:?}");
            assert!(
                failure
                    .message
                    .starts_with("kubectl api-resources printed "),
                "{}",
                failure.message
            );
            assert!(
                failure.message.contains(why),
                "{output:?}: {}",
                failure.message
            );
        }
        for name in [
            format!("{}a", label),
            format!("pods.{group}x"),
            format!("pods.{}.io", "c".repeat(64)),
        ] {
            let failure = words(format!("pods\n{name}\n").as_bytes()).unwrap_err();
            assert_eq!(failure.code, "cliProtocol", "{name}");
        }
        // A refused line is shortened in the reason, which a border shows.
        let failure = words(format!("{}\n", "X".repeat(500)).as_bytes()).unwrap_err();
        assert!(
            failure.message.contains('…') && failure.message.len() < 200,
            "{}",
            failure.message
        );
    }

    #[test]
    fn a_result_for_a_previous_context_or_kubeconfig_is_never_offered() {
        let (old, new) = (target("old-cluster"), target("new-cluster"));
        let mut kinds = ClusterKinds::default();
        assert!(kinds.wanted(&old));
        let first = kinds.begin(old.clone());
        // A running lookup is waited for, not started again.
        assert!(!kinds.wanted(&old));
        assert!(kinds.offered(&old).is_empty() && kinds.failure(&old).is_none());
        // The context changes while it runs: the caller is told to stop it.
        assert!(kinds.retarget(&new));
        assert!(kinds.wanted(&new));
        // Its result arrives anyway, and changes nothing.
        assert!(!kinds.finish(first, Listing::complete(strings(&["certificates"]))));
        assert!(kinds.offered(&old).is_empty() && kinds.offered(&new).is_empty());
        let second = kinds.begin(new.clone());
        assert_ne!(first, second);
        assert!(!kinds.finish(first, Listing::complete(strings(&["certificates"]))));
        assert!(kinds.finish(second, Listing::complete(strings(&["issuers"]))));
        assert_eq!(kinds.offered(&new), strings(&["issuers"]));
        assert!(kinds.offered(&old).is_empty());
        // Finished kinds are kept; a second result for them is stale too.
        assert!(!kinds.wanted(&new));
        assert!(!kinds.finish(second, Listing::complete(strings(&["other"]))));
        assert!(!kinds.retarget(&new));
        assert_eq!(kinds.offered(&new), strings(&["issuers"]));
        // A different kubeconfig is a different target with the same name.
        let other_config = Target {
            kubeconfig: Some("/fixture/other".into()),
            ..new.clone()
        };
        assert!(kinds.offered(&other_config).is_empty() && kinds.wanted(&other_config));
        // Leaving a target with finished kinds stops nothing and forgets them.
        assert!(!kinds.retarget(&other_config));
        assert!(kinds.offered(&new).is_empty() && kinds.wanted(&new));
        // A lookup that `begin` replaces without `retarget` is stale as well.
        let replaced = kinds.begin(old.clone());
        let current = kinds.begin(new.clone());
        assert!(!kinds.finish(replaced, Listing::complete(strings(&["certificates"]))));
        assert!(kinds.finish(current, Listing::complete(strings(&["issuers"]))));
        assert!(kinds.offered(&old).is_empty());
    }

    #[test]
    fn a_failed_lookup_is_shown_and_retried_by_the_next_prompt() {
        let (cluster, other) = (target("old-cluster"), target("new-cluster"));
        let mut kinds = ClusterKinds::default();
        let ticket = kinds.begin(cluster.clone());
        let message = |kinds: &ClusterKinds, target: &Target| {
            kinds.failure(target).map(|failure| failure.message.clone())
        };
        let reason = "kubectl exited exit status: 1: error: forbidden";
        assert!(kinds.finish(ticket, Listing::failed(Failure::new("cliError", reason))));
        assert_eq!(message(&kinds, &cluster).as_deref(), Some(reason));
        assert!(kinds.offered(&cluster).is_empty());
        assert_eq!(message(&kinds, &other), None);
        // The next prompt retries it; the reason stays until the retry ends.
        assert!(kinds.wanted(&cluster));
        let retry = kinds.begin(cluster.clone());
        assert!(!kinds.wanted(&cluster));
        assert_eq!(message(&kinds, &cluster).as_deref(), Some(reason));
        assert!(kinds.finish(retry, Listing::complete(strings(&["certificates"]))));
        assert_eq!(message(&kinds, &cluster), None);
        assert_eq!(kinds.offered(&cluster), strings(&["certificates"]));
        // A failure is not carried over to another target.
        let failed = kinds.begin(cluster.clone());
        assert!(kinds.finish(
            failed,
            Listing::failed(Failure::new("queryTimeout", "slow"))
        ));
        kinds.begin(other.clone());
        assert_eq!(
            (message(&kinds, &cluster), message(&kinds, &other)),
            (None, None)
        );
        // Nor kept after the target changes and comes back.
        let failed = kinds.begin(cluster.clone());
        assert!(kinds.finish(
            failed,
            Listing::failed(Failure::new("queryTimeout", "slow"))
        ));
        assert!(!kinds.retarget(&other));
        assert!(!kinds.retarget(&cluster));
        assert_eq!(message(&kinds, &cluster), None);
    }

    #[test]
    fn a_partial_listing_is_offered_reported_and_retried() {
        let (cluster, other) = (target("old-cluster"), target("new-cluster"));
        let mut kinds = ClusterKinds::default();
        let partly = "kubectl exited exit status: 1: error: unable to retrieve the complete list of server APIs: metrics.k8s.io/v1beta1: the server is currently unable to handle the request";
        let ticket = kinds.begin(cluster.clone());
        assert!(kinds.finish(
            ticket,
            Listing {
                words: strings(&["certificates"]),
                failure: Some(Failure::new("cliError", partly)),
            }
        ));
        // What kubectl listed is offered, and why the list may lack kinds is
        // reported with it.
        assert_eq!(kinds.offered(&cluster), strings(&["certificates"]));
        assert_eq!(
            kinds
                .failure(&cluster)
                .map(|failure| failure.message.as_str()),
            Some(partly)
        );
        assert_eq!(
            kinds.consequence(&cluster),
            "get completes only the kinds kubectl listed"
        );
        // The next prompt retries it; meanwhile both stay.
        assert!(kinds.wanted(&cluster));
        let retry = kinds.begin(cluster.clone());
        assert!(!kinds.wanted(&cluster));
        assert_eq!(kinds.offered(&cluster), strings(&["certificates"]));
        assert_eq!(
            kinds
                .failure(&cluster)
                .map(|failure| failure.message.as_str()),
            Some(partly)
        );
        // The retry's listing replaces it, even one that found nothing.
        let refused = "kubectl exited exit status: 1: The connection to the server 127.0.0.1:6443 was refused";
        assert!(kinds.finish(retry, Listing::failed(Failure::new("cliError", refused))));
        assert!(kinds.offered(&cluster).is_empty());
        assert_eq!(
            kinds
                .failure(&cluster)
                .map(|failure| failure.message.as_str()),
            Some(refused)
        );
        assert_eq!(
            kinds.consequence(&cluster),
            "get completes built-in kinds only"
        );
        // A complete listing clears the failure and is kept.
        let again = kinds.begin(cluster.clone());
        assert!(kinds.finish(
            again,
            Listing::complete(strings(&["certificates", "issuers"]))
        ));
        assert!(kinds.failure(&cluster).is_none() && !kinds.wanted(&cluster));
        assert_eq!(
            kinds.offered(&cluster),
            strings(&["certificates", "issuers"])
        );
        // A partial listing of another target is nothing to this one.
        let partial = kinds.begin(other.clone());
        assert!(kinds.finish(
            partial,
            Listing {
                words: strings(&["widgets"]),
                failure: Some(Failure::new("cliError", partly)),
            }
        ));
        assert!(kinds.offered(&cluster).is_empty() && kinds.failure(&cluster).is_none());
        assert!(kinds.wanted(&cluster));
    }

    #[test]
    fn the_reason_of_a_failed_lookup_is_one_line_in_kubectls_or_hamns_words() {
        let exited = |stderr: &str| {
            Failure::new(
                "cliError",
                format!("kubectl exited exit status: 1: {stderr}"),
            )
        };
        for (failure, expected) in [
            // kubectl's client log lines come before its own sentence when the
            // API server cannot be reached; the sentence is what is shown.
            (
                exited(concat!(
                    "E1015 01:17:26.408480   67207 memcache.go:265] \"Couldn't get current server API group list\" ",
                    "err=\"Get \\\"https://127.0.0.1:63621/api?timeout=32s\\\": dial tcp 127.0.0.1:63621: connect: connection refused\"\n",
                    "The connection to the server 127.0.0.1:63621 was refused - did you specify the right host or port?\n",
                )),
                "Cannot reach the Kubernetes API server at 127.0.0.1:63621: connection refused",
            ),
            (
                exited("error: You must be logged in to the server (Unauthorized)\n"),
                "Kubernetes rejected the credentials of this context",
            ),
            (
                exited(
                    "Error from server (Forbidden): the server does not allow this method on the requested resource\n",
                ),
                "The server does not allow this method on the requested resource",
            ),
            // A partial discovery names the groups it could not list.
            (
                exited(
                    "error: unable to retrieve the complete list of server APIs: metrics.k8s.io/v1beta1: the server is currently unable to handle the request\n",
                ),
                "These API groups could not be listed: metrics.k8s.io/v1beta1: the server is currently unable to handle the request",
            ),
            (
                exited(""),
                "kubectl failed without an error message (exit status: 1)",
            ),
            (
                Failure::new(
                    "cliUnavailable",
                    "kubectl: No such file or directory (os error 2)",
                ),
                "kubectl is not installed or not on PATH",
            ),
            // Hamn's own reasons stay as they are, whatever their code.
            (
                Failure::new(
                    "queryTimeout",
                    "kubectl api-resources did not finish in 30s and was stopped",
                ),
                "kubectl api-resources did not finish in 30s and was stopped",
            ),
            (
                words(b"ACTION_DONE\n").unwrap_err(),
                "kubectl api-resources printed \"ACTION_DONE\", which is not a resource name",
            ),
        ] {
            assert_eq!(reason(&failure), expected, "{failure:?}");
        }
    }

    #[test]
    fn the_lookup_names_only_the_targets_kubeconfig_and_context() {
        let args = |target: &Target| -> Vec<String> {
            command(target)
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let program = command(&target("dev")).as_std().get_program().to_owned();
        assert_eq!(program, "kubectl");
        let none = Target {
            kubeconfig: None,
            context: None,
        };
        assert_eq!(args(&none), strings(&["api-resources", "-o", "name"]));
        assert_eq!(
            args(&Target {
                kubeconfig: Some("/fixture/config".into()),
                context: Some("dev".into())
            }),
            strings(&[
                "--kubeconfig",
                "/fixture/config",
                "--context",
                "dev",
                "api-resources",
                "-o",
                "name"
            ])
        );
    }

    /// `names`, `garbage` (not only names, exit 0), `fail` (names, then
    /// exit 1 with an error), `garbage-fail`, `empty-fail`, `flood`, or
    /// `hang LEADER MEMBER`, which writes the PIDs of itself and of a child in
    /// its process group to those files and waits for a signal in both.
    const PEER_SOURCE: &str = r#"
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void write_pid(const char *path) {
    char partial[4096];
    if (snprintf(partial, sizeof partial, "%s.partial", path) >= (int)sizeof partial) exit(2);
    FILE *file = fopen(partial, "w");
    if (file == NULL || fprintf(file, "%d\n", (int)getpid()) < 0 || fclose(file) != 0) exit(2);
    if (rename(partial, path) != 0) exit(2);
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "names") == 0) {
        fputs("pods\ncertificates.cert-manager.io\n", stdout);
        return 0;
    }
    if (argc == 2 && strcmp(argv[1], "garbage") == 0) {
        fputs("NAME\npods\n", stdout);
        return 0;
    }
    if (argc == 2 && (strcmp(argv[1], "fail") == 0 || strcmp(argv[1], "garbage-fail") == 0
                      || strcmp(argv[1], "empty-fail") == 0)) {
        if (strcmp(argv[1], "fail") == 0) fputs("pods\n", stdout);
        if (strcmp(argv[1], "garbage-fail") == 0) fputs("NAME\npods\n", stdout);
        fputs("\n  error: unable to retrieve the complete list of server APIs  \nmore\n", stderr);
        return 1;
    }
    if (argc == 2 && strcmp(argv[1], "flood") == 0) {
        static char line[4096];
        memset(line, 'x', sizeof line);
        for (;;) {
            if (fwrite(line, 1, sizeof line, stdout) != sizeof line) return 3;
        }
    }
    if (argc == 4 && strcmp(argv[1], "hang") == 0) {
        pid_t child = fork();
        if (child < 0) return 2;
        write_pid(child == 0 ? argv[3] : argv[2]);
        for (;;) pause();
    }
    return 64;
}
"#;

    fn peer(name: &str) -> crate::test_fixture::CFixture {
        crate::test_fixture::CFixture::compile(&format!("cluster-kinds-{name}"), PEER_SOURCE, &[])
    }

    fn peer_command(
        fixture: &crate::test_fixture::CFixture,
        args: &[&std::ffi::OsStr],
    ) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(fixture.program());
        command.args(args);
        command
    }

    #[tokio::test]
    async fn a_lookup_offers_the_names_kubectl_listed_even_when_it_failed() {
        let fixture = peer("results");
        let run = |mode: &str| {
            listing(
                peer_command(&fixture, &[mode.as_ref()]),
                Duration::from_secs(20),
            )
        };
        let complete = run("names").await;
        assert_eq!(complete.words, strings(&["pods", "certificates"]));
        assert!(complete.failure.is_none(), "{complete:?}");
        // A partial discovery: names on stdout and exit 1. The names are
        // offered, and the failure is kept whole, in the form
        // `native::query` reports an exit; the border shows its first
        // meaningful line.
        let stderr = "\n  error: unable to retrieve the complete list of server APIs  \nmore\n";
        let partial = run("fail").await;
        assert_eq!(partial.words, strings(&["pods"]));
        let failure = partial.failure.as_ref().unwrap();
        assert_eq!(failure.code, "cliError");
        assert_eq!(
            failure.message,
            format!("kubectl exited exit status: 1: {stderr}")
        );
        assert_eq!(
            reason(failure),
            "kubectl: unable to retrieve the complete list of server APIs"
        );
        // Stdout that is not a list of names is not offered after exit 1,
        // and neither is empty stdout; the failure is the exit's either way.
        for mode in ["garbage-fail", "empty-fail"] {
            let failed = run(mode).await;
            assert!(failed.words.is_empty(), "{mode}: {failed:?}");
            let failure = failed.failure.as_ref().unwrap();
            assert_eq!(failure.code, "cliError", "{mode}");
            assert_eq!(
                failure.message,
                format!("kubectl exited exit status: 1: {stderr}"),
                "{mode}"
            );
        }
        // After exit 0, stdout that is not a list of names is refused.
        let refused = run("garbage").await;
        assert!(refused.words.is_empty(), "{refused:?}");
        assert_eq!(refused.failure.as_ref().unwrap().code, "cliProtocol");
        let missing = listing(
            tokio::process::Command::new(fixture.program().with_file_name("missing")),
            Duration::from_secs(20),
        )
        .await;
        assert!(missing.words.is_empty());
        let missing = missing.failure.unwrap();
        assert_eq!(missing.code, "cliUnavailable");
        assert_eq!(reason(&missing), "kubectl is not installed or not on PATH");
        assert!(
            missing.message.starts_with("kubectl: "),
            "{}",
            missing.message
        );
    }

    #[tokio::test]
    async fn output_past_the_limit_fails_without_waiting_for_the_deadline() {
        let fixture = peer("flood");
        let started = std::time::Instant::now();
        let flooded = listing(
            peer_command(&fixture, &["flood".as_ref()]),
            Duration::from_secs(60),
        )
        .await;
        assert!(flooded.words.is_empty(), "{flooded:?}");
        let failure = flooded.failure.unwrap();
        assert_eq!(failure.code, "cliOutput");
        assert_eq!(
            failure.message,
            "kubectl api-resources: output exceeds 1024 KiB"
        );
        assert_eq!(reason(&failure), failure.message);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "{:?}",
            started.elapsed()
        );
    }

    fn gone(pid: libc::pid_t) -> bool {
        (unsafe { libc::kill(pid, 0) }) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[tokio::test]
    async fn a_lookup_past_its_deadline_kills_its_process_group_and_reaps_it() {
        let fixture = peer("deadline");
        let leader_file = fixture.program().with_file_name("leader");
        let member_file = fixture.program().with_file_name("member");
        let command = peer_command(
            &fixture,
            &[
                "hang".as_ref(),
                leader_file.as_os_str(),
                member_file.as_os_str(),
            ],
        );
        let lookup = tokio::spawn(listing(command, Duration::from_secs(3)));
        let read = |path: &std::path::Path| -> Option<libc::pid_t> {
            std::fs::read_to_string(path).ok()?.trim().parse().ok()
        };
        let started = std::time::Instant::now();
        let (leader, member) = loop {
            if let (Some(leader), Some(member)) = (read(&leader_file), read(&member_file)) {
                break (leader, member);
            }
            assert!(
                !lookup.is_finished(),
                "the peer did not start before the deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(!gone(leader) && !gone(member));
        let stopped = lookup.await.unwrap();
        assert!(stopped.words.is_empty(), "{stopped:?}");
        let failure = stopped.failure.unwrap();
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(failure.code, "queryTimeout");
        assert_eq!(
            failure.message,
            "kubectl api-resources did not finish in 3s and was stopped"
        );
        // The leader is reaped before the lookup returns, so its PID is free.
        assert!(gone(leader), "leader {leader} outlived the lookup");
        // The member died with the group; launchd reaps the orphan.
        let reaped = std::time::Instant::now() + Duration::from_secs(10);
        while !gone(member) {
            assert!(
                std::time::Instant::now() < reaped,
                "member {member} outlived its group"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
