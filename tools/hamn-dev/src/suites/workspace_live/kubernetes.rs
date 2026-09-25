//! A disposable kind cluster in the owned Hamn Docker environment: live
//! Kubernetes queries, apply, interactive exec and port-forward through the
//! TUI, guarded delete and restart against API replacement and concurrent
//! changes, and the management review. The source kubeconfig must stay
//! byte for byte unchanged.
use super::management::management_review;
use super::terminal::{Driver, Terminal, exercise};
use super::{Live, Must, finally, path_str, run_env, write_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

const CLUSTER: &str = "hamn-workspace-proof";

/// A rejected restart must preserve the selected object's desired state.
/// Controllers may change its status, metadata and resourceVersion between
/// reads, so only the identity, the spec and the concurrent edit are
/// compared.
pub(crate) fn assert_deployment_preserved(before: &Value, after: &Value) -> Result<(), String> {
    if after["metadata"]["uid"] != before["metadata"]["uid"] {
        return Err("the deployment was replaced".into());
    }
    let (Some(expected), Some(observed)) = (before.get("spec"), after.get("spec")) else {
        return Err("a deployment snapshot has no spec".into());
    };
    if expected != observed {
        return Err("the deployment's desired state changed".into());
    }
    let proof = |value: &Value| value["metadata"]["annotations"].get("guard-proof").cloned();
    match (proof(before), proof(after)) {
        (Some(expected), Some(observed)) if expected == observed => Ok(()),
        (None, _) => Err("the expected deployment has no guard-proof annotation".into()),
        _ => Err("the concurrent guard-proof edit was lost".into()),
    }
}

/// Menu preconditions against API replacement and concurrent changes.
struct Guarded<'a> {
    live: &'a Live,
    config: &'a Path,
    environment: &'a BTreeMap<String, String>,
    name: String,
    evidence: RefCell<Value>,
    owned: Cell<bool>,
}

impl Guarded<'_> {
    fn kubectl(&self, args: &[&str]) -> String {
        let all = [&["--kubeconfig", path_str(self.config)][..], args].concat();
        run_env("kubectl", &all, self.environment, Duration::from_secs(660), None).must()
    }

    fn save(&self) {
        write_json(&self.live.root.join("kubernetes-guarded-actions.json"), &self.evidence.borrow());
    }

    fn record(&self, key: &str, value: Value) {
        self.evidence.borrow_mut()[key] = value;
    }

    fn recorded(&self, key: &str) -> Value {
        self.evidence.borrow()[key].clone()
    }

    fn resource(&self, kind: &str, name: &str) -> Value {
        let mut args = vec!["get", kind, name, "-o", "json"];
        if kind != "namespace" {
            args.extend(["--namespace", &self.name]);
        }
        let value: Value = serde_json::from_str(&self.kubectl(&args)).expect("kubectl JSON");
        let mut evidence = self.evidence.borrow_mut();
        let snapshots =
            evidence.as_object_mut().expect("evidence object").entry("resourceSnapshots").or_insert(json!([]));
        snapshots.as_array_mut().expect("snapshots").push(json!({"kind": kind, "resource": value}));
        drop(evidence);
        self.save();
        value
    }

    fn create_namespace(&self) -> Value {
        self.kubectl(&["create", "namespace", &self.name]);
        self.owned.set(true);
        self.kubectl(&[
            "wait",
            "--for=jsonpath={.status.phase}=Active",
            &format!("namespace/{}", self.name),
            "--timeout=60s",
        ]);
        self.resource("namespace", &self.name)
    }

    fn create_deployment(&self, manifest: &Path) -> Value {
        self.kubectl(&["create", "-f", path_str(manifest)]);
        self.kubectl(&[
            "rollout",
            "status",
            &format!("deployment/{}", self.name),
            "--namespace",
            &self.name,
            "--timeout=60s",
        ]);
        self.resource("deployment", &self.name)
    }

    /// Selects `kind/resource` in a TUI that starts without cached rows,
    /// opens the confirmation for `key`, runs `change` while refresh is
    /// paused, confirms, and expects `Exit code CODE`.
    fn confirmed_action(&self, kind: &str, resource: &str, key: &[u8], action: &str, change: &dyn Fn(), code: i32) {
        let mut terminal = Terminal::new(&self.live.runtime.binary, self.environment, &self.live.root);
        finally(
            &mut terminal,
            |terminal| {
                if !self.live.runtime.home.join(".hamn/tui.json").exists() {
                    terminal.until("Choose your default workspace");
                    terminal.send(b"1\r", None);
                }
                terminal.until("hamn-workspace-sentinel");
                let mut query = format!("kubectl get {kind} {resource}");
                if kind != "namespaces" {
                    query += &format!(" --namespace {}", self.name);
                }
                terminal.send(format!(":{query}\r").as_bytes(), Some(&format!("> {resource}")));
                terminal.send(key, Some(&format!("Confirm {action} {kind} {resource}")));
                change();
                terminal.send(b"y", Some(&format!("Exit code {code}")));
                if action == "delete" && code == 1 {
                    assert!(terminal.text().contains("Conflict"), "{}", terminal.text());
                }
                terminal.send(b"\r", None);
            },
            |terminal| {
                // Esc dismisses an unfinished confirmation or returns from
                // an exited CLI, so a failure does not leave that view.
                if terminal.running() {
                    terminal.write(b"\x1b");
                }
                terminal.close();
            },
        );
    }
}

fn guarded_mutations(live: &Live, config: &Path, environment: &BTreeMap<String, String>) {
    let digest: String =
        Sha256::digest(path_str(&live.root).as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    let guarded = Guarded {
        live,
        config,
        environment,
        name: format!("hamn-guard-{}", &digest[..12]),
        evidence: RefCell::new(json!({})),
        owned: Cell::new(false),
    };
    let name = guarded.name.clone();
    let mut nothing = ();
    finally(
        &mut nothing,
        |_| {
            let original = guarded.create_namespace();
            guarded.record("originalNamespaceUid", original["metadata"]["uid"].clone());
            let replace_namespace = || {
                guarded.kubectl(&["delete", "namespace", &name, "--wait=true", "--timeout=60s"]);
                let replacement = guarded.create_namespace();
                guarded.record("replacementNamespaceUid", replacement["metadata"]["uid"].clone());
                assert_ne!(guarded.recorded("replacementNamespaceUid"), guarded.recorded("originalNamespaceUid"));
            };
            guarded.confirmed_action("namespaces", &name, b"d", "delete", &replace_namespace, 1);
            assert_eq!(
                guarded.resource("namespace", &name)["metadata"]["uid"],
                guarded.recorded("replacementNamespaceUid")
            );
            guarded.confirmed_action("namespaces", &name, b"d", "delete", &|| {}, 0);
            guarded.kubectl(&["wait", "--for=delete", &format!("namespace/{name}"), "--timeout=60s"]);
            guarded.owned.set(false);
            guarded.record("replacementPreservedThenFreshDeleteSucceeded", json!(true));

            guarded.create_namespace();
            let deployment = json!({"apiVersion": "apps/v1", "kind": "Deployment",
                "metadata": {"name": name, "namespace": name}, "spec": {"replicas": 0,
                "selector": {"matchLabels": {"app": name}}, "template": {
                "metadata": {"labels": {"app": name}, "annotations": {"keep": "preserved"}},
                "spec": {"containers": [{"name": "idle", "image": "busybox:1.37"}]}}}});
            let manifest = live.root.join("guarded-deployment.json");
            fs::write(&manifest, deployment.to_string()).unwrap();
            let original = guarded.create_deployment(&manifest);
            guarded.record("originalDeploymentUid", original["metadata"]["uid"].clone());
            let replace_deployment = || {
                guarded.kubectl(&["delete", "deployment", &name, "--namespace", &name, "--wait=true", "--timeout=60s"]);
                let replacement = guarded.create_deployment(&manifest);
                guarded.record("replacementDeploymentUid", replacement["metadata"]["uid"].clone());
                assert_ne!(guarded.recorded("replacementDeploymentUid"), guarded.recorded("originalDeploymentUid"));
            };
            guarded.confirmed_action("deployments", &name, b"r", "restart", &replace_deployment, 1);
            let replacement = guarded.resource("deployment", &name);
            assert_eq!(replacement["metadata"]["uid"], guarded.recorded("replacementDeploymentUid"));
            let restarted_at = |value: &Value| {
                value["spec"]["template"]["metadata"]["annotations"].get("kubectl.kubernetes.io/restartedAt").cloned()
            };
            assert_eq!(restarted_at(&replacement), None, "a stale restart reached the replacement");

            let concurrent_expected = RefCell::new(Value::Null);
            let change_version = || {
                guarded.kubectl(&["annotate", "deployment", &name, "--namespace", &name, "guard-proof=changed"]);
                let value = guarded.resource("deployment", &name);
                guarded.record("concurrentVersion", value["metadata"]["resourceVersion"].clone());
                *concurrent_expected.borrow_mut() = value;
            };
            guarded.confirmed_action("deployments", &name, b"r", "restart", &change_version, 1);
            let concurrent = guarded.resource("deployment", &name);
            guarded.record("afterRejectedVersion", concurrent["metadata"]["resourceVersion"].clone());
            assert_deployment_preserved(&concurrent_expected.borrow(), &concurrent).must();
            assert_eq!(restarted_at(&concurrent), None, "a restart of a changed version was applied");
            guarded.confirmed_action("deployments", &name, b"r", "restart", &|| {}, 0);
            let restarted = guarded.resource("deployment", &name);
            assert_eq!(restarted["metadata"]["uid"], guarded.recorded("replacementDeploymentUid"));
            let annotations = &restarted["spec"]["template"]["metadata"]["annotations"];
            assert!(
                annotations["keep"] == "preserved"
                    && annotations["kubectl.kubernetes.io/restartedAt"].as_str().is_some_and(|at| !at.is_empty()),
                "{annotations}"
            );
            guarded.record("replacementAndConcurrentVersionRejectedThenFreshRestartSucceeded", json!(true));
            guarded.save();
            println!(
                "PASS: real Kubernetes guarded delete/restart reject stale UID/version and accept fresh selection"
            );
        },
        |_| {
            guarded.save();
            if guarded.owned.get() {
                guarded.kubectl(&[
                    "delete",
                    "namespace",
                    &name,
                    "--ignore-not-found=true",
                    "--wait=true",
                    "--timeout=60s",
                ]);
            }
        },
    );
}

/// Creates (with `create`) the disposable kind cluster in the owned engine
/// and runs every Kubernetes check against it; the cluster is always
/// deleted.
pub(crate) fn kubernetes(live: &Live, create: bool) {
    let config = live.root.join("kubeconfig");
    let socket = format!("unix://{}", live.profile().join("docker.sock").display());
    let environment = live.environment(&[("DOCKER_HOST", &socket), ("KUBECONFIG", path_str(&config))]);
    let kubectl = |args: &[&str]| {
        let all = [&["--kubeconfig", path_str(&config)][..], args].concat();
        run_env("kubectl", &all, &environment, Duration::from_secs(660), None).must()
    };
    let kind = |args: &[&str]| {
        run_env("kind", args, &environment, Duration::from_secs(660), None).must();
    };
    let mut nothing = ();
    finally(
        &mut nothing,
        |_| {
            if create {
                kind(&["create", "cluster", "--name", CLUSTER, "--kubeconfig", path_str(&config), "--wait", "120s"]);
            }
            kubectl(&["create", "namespace", "workspace-proof"]);
            kubectl(&["config", "set-context", "--current", "--namespace=workspace-proof"]);
            let pod = json!({"apiVersion": "v1", "kind": "Pod",
                "metadata": {"name": "workspace-http", "namespace": "workspace-proof"},
                "spec": {"containers": [{"name": "http", "image": "busybox:1.37", "imagePullPolicy": "IfNotPresent",
                    "command": ["sh", "-c", "mkdir -p /www; echo kube-http-proof > /www/index.html; exec httpd -f -p 8080 -h /www"]}]}});
            let manifest = live.root.join("pod.json");
            fs::write(&manifest, pod.to_string()).unwrap();
            kubectl(&["apply", "-f", path_str(&manifest)]);
            kubectl(&[
                "wait",
                "-n",
                "workspace-proof",
                "--for=condition=Ready",
                "pod/workspace-http",
                "--timeout=120s",
            ]);
            let configmap = json!({"apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {"name": "tui-proof", "namespace": "workspace-proof"}, "data": {"proof": "preserved-cli"}});
            fs::write(live.root.join("configmap.json"), configmap.to_string()).unwrap();
            let before = fs::read(&config).unwrap();
            exercise(live, Some(&config));
            assert_eq!(fs::read(&config).unwrap(), before, "TUI selection changed kubeconfig");
            assert!(
                kubectl(&["get", "configmap", "tui-proof", "-n", "workspace-proof", "-o", "json"])
                    .contains("preserved-cli"),
                "the TUI's apply did not reach the cluster"
            );
            guarded_mutations(live, &config, &environment);
            management_review(live, &config, &environment, CLUSTER);
            fs::write(live.root.join("kubernetes-version.json"), kubectl(&["version", "-o", "json"])).unwrap();
            println!("PASS: live Kubernetes query, apply, interactive exec, port-forward and unchanged kubeconfig");
        },
        |_| kind(&["delete", "cluster", "--name", CLUSTER, "--kubeconfig", path_str(&config)]),
    );
}
