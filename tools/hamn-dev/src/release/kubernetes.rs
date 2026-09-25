//! `release external-kubernetes-e2e`: validates a supplied Hamn binary in a
//! disposable namespace of an explicitly selected cluster.
//!
//! kubectl owns fixture setup and cleanup only; every operation under test
//! goes through Hamn. The namespace is removed and the original kubeconfig
//! bytes are compared even when a check fails. Evidence is written only
//! after every check passed, to a path that must not exist yet.
use super::files::{canonical_json, sha256_file, write_new};
use super::process::{self, Spec};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "usage: hamn-dev release external-kubernetes-e2e --hamn HAMN --context CONTEXT \
[--kubeconfig KUBECONFIG] --output OUTPUT [--host-network]

Validate a supplied Hamn binary in a disposable namespace of an explicit cluster.
kubectl owns fixture setup/cleanup only. Every operation under test uses Hamn.
The original kubeconfig bytes and current-context are never changed.

  --host-network  Use host networking for API/log tests when the test cluster
                  CNI is unavailable; this does not validate Pod networking";

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub hamn: PathBuf,
    pub context: String,
    pub kubeconfig: Option<PathBuf>,
    pub output: PathBuf,
    pub host_network: bool,
}

/// `None` when help was requested.
pub fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let (mut hamn, mut context, mut kubeconfig, mut output, mut host_network) = (None, None, None, None, false);
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (word.as_str(), None),
        };
        let mut value =
            || inline.clone().or_else(|| words.next().cloned()).ok_or(format!("{flag} requires a value\n{USAGE}"));
        match flag {
            "-h" | "--help" => return Ok(None),
            "--hamn" => hamn = Some(PathBuf::from(value()?)),
            "--context" => context = Some(value()?),
            "--kubeconfig" => kubeconfig = Some(PathBuf::from(value()?)),
            "--output" => output = Some(PathBuf::from(value()?)),
            "--host-network" if inline.is_none() => host_network = true,
            _ => return Err(format!("unrecognized argument {word:?}\n{USAGE}")),
        }
    }
    match (hamn, context, output) {
        (Some(hamn), Some(context), Some(output)) => {
            Ok(Some(Options { hamn, context, kubeconfig, output, host_network }))
        }
        _ => Err(format!("--hamn, --context and --output are required\n{USAGE}")),
    }
}

pub fn main(args: &[String]) -> Result<(), String> {
    match parse(args)? {
        None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(options) => {
            run(&options)?;
            println!("External Kubernetes lifecycle, logs, identity checks and cleanup: passed");
            Ok(())
        }
    }
}

/// The kubeconfig files whose bytes must not change: the explicit file, or
/// every entry of `$KUBECONFIG`, or `~/.kube/config`.
fn kubeconfig_paths(options: &Options) -> Vec<PathBuf> {
    if let Some(path) = &options.kubeconfig {
        return vec![path.clone()];
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let list = std::env::var_os("KUBECONFIG").unwrap_or_else(|| home.join(".kube/config").into_os_string());
    std::env::split_paths(&list).filter(|path| !path.as_os_str().is_empty()).collect()
}

fn unique_namespace() -> Result<String, String> {
    let mut random = [0u8; 8];
    fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut random))
        .map_err(|error| error.to_string())?;
    Ok(format!("hamn-e2e-{}", random.iter().map(|byte| format!("{byte:02x}")).collect::<String>()))
}

struct Check<'a> {
    kube: Vec<OsString>,
    hamn: Vec<OsString>,
    namespace: &'a str,
    checks: BTreeSet<String>,
}

impl Check<'_> {
    fn run(&self, command: &[OsString], input: Option<&[u8]>, timeout: u64) -> Result<String, String> {
        let (program, args) = command.split_first().expect("command has a program");
        process::run(program, args, &Spec { environment: None, input }, Duration::from_secs(timeout))
    }

    fn kubectl(&self, words: &[&str], input: Option<&[u8]>, timeout: u64) -> Result<String, String> {
        let mut command = self.kube.clone();
        command.extend(words.iter().map(OsString::from));
        self.run(&command, input, timeout)
    }

    /// A Hamn operation under test; `flags` follow the words.
    fn operation(&mut self, words: &[&str], flags: &[&str]) -> Result<Value, String> {
        let mut command = self.hamn.clone();
        command.extend(words.iter().chain(flags).map(OsString::from));
        let result: Value = serde_json::from_str(&self.run(&command, None, 90)?).map_err(|error| error.to_string())?;
        if result.get("schemaVersion") != Some(&json!(1)) || result.get("ok") != Some(&json!(true)) {
            return Err(format!("invalid Hamn response: {result}"));
        }
        self.checks.insert(words.iter().take(3).copied().collect::<Vec<_>>().join(" "));
        Ok(result.get("data").cloned().unwrap_or(Value::Null))
    }

    fn rollout(&self, resource: &str, name: &str) -> Result<(), String> {
        self.kubectl(
            &["-n", self.namespace, "rollout", "status", &format!("{resource}/{name}"), "--timeout=180s"],
            None,
            210,
        )
        .map(drop)
    }

    fn exercise(&mut self, host_network: bool) -> Result<(), String> {
        let namespace = self.namespace.to_owned();
        let items: Vec<Value> = [("Deployment", "deployment"), ("StatefulSet", "statefulset"), ("DaemonSet", "daemonset")]
            .into_iter()
            .map(|(kind, name)| {
                let labels = json!({"app": name});
                let mut spec = json!({
                    "selector": {"matchLabels": labels},
                    "template": {"metadata": {"labels": labels}, "spec": {
                        "terminationGracePeriodSeconds": 1,
                        "hostNetwork": host_network,
                        "containers": [{"name": "logger", "image": "busybox:1.37",
                            "command": ["sh", "-c", "echo hamn-kubernetes-e2e; sleep 3600"],
                            "resources": {"requests": {"cpu": "10m", "memory": "8Mi"},
                                          "limits": {"cpu": "100m", "memory": "32Mi"}}}]}},
                });
                if kind != "DaemonSet" {
                    spec["replicas"] = json!(1);
                }
                if kind == "StatefulSet" {
                    spec["serviceName"] = json!("statefulset");
                }
                json!({"apiVersion": "apps/v1", "kind": kind, "metadata": {"name": name, "namespace": namespace}, "spec": spec})
            })
            .collect();
        let list = json!({"apiVersion": "v1", "kind": "List", "items": items}).to_string();
        self.kubectl(&["-n", &namespace, "create", "-f", "-"], Some(list.as_bytes()), 90)?;
        for kind in ["deployment", "statefulset", "daemonset"] {
            self.rollout(kind, kind)?;
        }
        for resource in [
            "namespaces",
            "nodes",
            "pods",
            "deployments",
            "statefulsets",
            "daemonsets",
            "services",
            "events",
            "jobs",
            "cronjobs",
            "ingresses",
            "pvcs",
        ] {
            if !self.operation(&["k8s", resource, "list"], &[])?.is_array() {
                return Err(format!("k8s {resource} list did not return a list"));
            }
        }
        for (resource, name) in [("deployments", "deployment"), ("statefulsets", "statefulset")] {
            for count in [3, 1] {
                self.operation(&["k8s", resource, "scale", name], &["--replicas", &count.to_string(), "--yes"])?;
                let detail = self.operation(&["k8s", resource, "inspect", name], &[])?;
                if detail["object"]["spec"]["replicas"] != json!(count) {
                    return Err(format!("{resource}/{name} did not scale to {count}: {detail}"));
                }
            }
        }
        for (resource, name) in
            [("deployments", "deployment"), ("statefulsets", "statefulset"), ("daemonsets", "daemonset")]
        {
            self.operation(&["k8s", resource, "restart", name], &["--yes"])?;
            let detail = self.operation(&["k8s", resource, "inspect", name], &[])?;
            let annotated = detail["object"]["spec"]["template"]["metadata"]["annotations"]
                .as_object()
                .is_some_and(|map| !map.is_empty());
            if detail["yaml"].as_str().is_none_or(str::is_empty) || !annotated {
                return Err(format!("{resource}/{name} restart left no annotated template: {detail}"));
            }
            self.rollout(resource, name)?;
        }
        let pods = self.operation(&["k8s", "pods", "list"], &[])?;
        let selected = pods
            .as_array()
            .and_then(|pods| pods.iter().find(|pod| pod["metadata"]["labels"]["app"] == "deployment"))
            .ok_or("no deployment Pod is listed")?;
        let (Some(name), Some(uid)) = (selected["metadata"]["name"].as_str(), selected["metadata"]["uid"].as_str())
        else {
            return Err(format!("deployment Pod has no name or UID: {selected}"));
        };
        let (name, uid) = (name.to_owned(), uid.to_owned());
        let mut logs = self.hamn.clone();
        logs.extend(["k8s", "pods", "logs", &name, "--container", "logger", "--tail", "10"].map(OsString::from));
        let records = self
            .run(&logs, None, 90)?
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        if !records.iter().any(|record| record.to_string().contains("hamn-kubernetes-e2e")) {
            return Err(format!("Pod logs lack the fixture line: {records:?}"));
        }
        self.checks.insert("k8s pods logs".into());
        self.operation(&["k8s", "pods", "delete", &name], &["--uid", &uid, "--yes"])?;
        let mut stale = self.hamn.clone();
        stale.extend(["k8s", "pods", "delete", &name, "--uid", &format!("{uid}-stale"), "--yes"].map(OsString::from));
        let (program, args) = stale.split_first().expect("command has a program");
        let rejected = process::capture(program, args, &Spec::default(), Duration::from_secs(90))?;
        let refused = serde_json::from_slice::<Value>(&rejected.stdout).ok().and_then(|value| value.get("ok").cloned());
        if rejected.status.success() || refused != Some(json!(false)) {
            return Err("a delete with a stale Pod UID was not rejected".into());
        }
        self.checks.insert("stalePodRejected".into());
        Ok(())
    }
}

pub fn run(options: &Options) -> Result<Value, String> {
    let binary = options.hamn.canonicalize().map_err(|error| format!("{}: {error}", options.hamn.display()))?;
    if fs::symlink_metadata(&options.output).is_ok() {
        return Err("evidence output already exists".into());
    }
    let namespace = unique_namespace()?;
    let before = kubeconfig_paths(options)
        .into_iter()
        .map(|path| {
            fs::read(&path).map(|data| (path.clone(), data)).map_err(|error| format!("{}: {error}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let binary_sha256 = sha256_file(&binary)?;
    let mut kube: Vec<OsString> =
        ["kubectl", "--context", &options.context, "--request-timeout=30s"].map(OsString::from).into();
    let mut hamn: Vec<OsString> = vec![binary.clone().into()];
    hamn.extend(
        ["--headless", "--context", &options.context, "--namespace", &namespace, "--timeout", "60"].map(OsString::from),
    );
    if let Some(path) = &options.kubeconfig {
        kube.extend([OsString::from("--kubeconfig"), path.clone().into()]);
        hamn.extend([OsString::from("--kubeconfig"), path.clone().into()]);
    }
    let mut check = Check { kube, hamn, namespace: &namespace, checks: BTreeSet::new() };
    let mut outcome = check.kubectl(&["create", "namespace", &namespace], None, 90).map(drop);
    let created = outcome.is_ok();
    if created {
        outcome = check.exercise(options.host_network);
    }
    let mut failures: Vec<String> = outcome.err().into_iter().collect();
    if created
        && let Err(error) =
            check.kubectl(&["delete", "namespace", &namespace, "--wait=true", "--timeout=120s"], None, 150)
    {
        failures.push(error);
    }
    if before.iter().any(|(path, data)| fs::read(path).ok().as_ref() != Some(data)) {
        failures.push("kubeconfig changed during validation".into());
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    if sha256_file(&binary)? != binary_sha256 {
        return Err("binary changed during validation".into());
    }
    let value = json!({
        "schemaVersion": 1,
        "kind": "hamn-external-kubernetes-e2e",
        "passed": true,
        "binarySha256": binary_sha256,
        "context": options.context,
        "namespace": namespace,
        "podNetwork": if options.host_network { "host" } else { "cluster" },
        "namespaceRemoved": true,
        "kubeconfigUnchanged": true,
        "checks": check.checks.into_iter().collect::<Vec<_>>(),
    });
    write_new(&options.output, canonical_json(&value).as_bytes())?;
    Ok(value)
}

/// The external harness as a child of this executable, bounded as the
/// physical gate requires.
pub fn run_child(options: &Options, timeout: Duration) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut args: Vec<OsString> = ["release", "external-kubernetes-e2e", "--hamn"].map(OsString::from).into();
    args.push(options.hamn.clone().into());
    args.extend(["--context".into(), OsString::from(&options.context)]);
    if let Some(path) = &options.kubeconfig {
        args.extend(["--kubeconfig".into(), path.clone().into()]);
    }
    args.extend(["--output".into(), options.output.clone().into()]);
    if options.host_network {
        args.push("--host-network".into());
    }
    process::run(executable.as_os_str(), &args, &Spec::default(), timeout).map(drop)
}
