//! The installed kubectl dispatches `create` extensions once, preserving the
//! plugin argv and exit code; installed plugin names never displace built-in
//! commands or aliases, and built-ins and literal data keep the UI
//! namespace without redirecting the kubeconfig reload.
use super::tui_cluster_target::assert_hamn_directory_holds_preferences_only;
use crate::runner::{self, case};
use crate::support::py_text::{json_list, splitlines, strip};
use crate::support::real_cli::{self, Output, recorded_argv, run};
use crate::support::tui::{self, Harness, install_fixture};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

/// These installed names must never displace a built-in command or alias.
const BUILTINS: [&str; 27] = [
    "clusterrole",
    "clusterrolebinding",
    "configmap",
    "cm",
    "cronjob",
    "cj",
    "deployment",
    "deploy",
    "ingress",
    "ing",
    "job",
    "namespace",
    "ns",
    "poddisruptionbudget",
    "pdb",
    "priorityclass",
    "pc",
    "quota",
    "resourcequota",
    "role",
    "rolebinding",
    "secret",
    "service",
    "svc",
    "serviceaccount",
    "sa",
    "token",
];

pub fn main(filters: &[String]) -> ExitCode {
    let Some(kubectl) = real_cli::which("kubectl") else {
        println!("SKIP: installed kubectl unavailable for create plugin dispatch");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "tui-create-plugins",
        "create plugins retain exact argv/exit/count; built-ins, aliases and literal data keep UI namespace",
        vec![case("main", move || create_plugins(&kubectl))],
        filters,
    )
}

/// Installs `root/bin/<program>` running `fixture`.
fn install(root: &Path, program: &str, fixture: &str) {
    install_fixture(&root.join("bin"), program);
    real_cli::select(root, program, fixture);
}

fn create_plugins(kubectl: &Path) {
    let mut harness = Harness::with(tui::Options { namespace: Some("ui-ns"), ..tui::Options::new("kubernetes") });
    harness.until("old-target-row");
    let root = harness.root.clone();
    install(&root, "kubectl-create-hamnfixture", "create-plugin");
    for name in std::iter::once(String::new()).chain(BUILTINS.iter().map(|name| format!("-{name}"))) {
        install(&root, &format!("kubectl-create{name}"), "create-shadow-plugin");
    }
    real_cli::wrap(&root, "kubectl", kubectl, "create-plugins-kubectl");
    // The Python test's environment for direct runs, plus FIXTURE_ROOT: the
    // plugins are links to this executable (not scripts) and find their
    // fixture selection there.
    let direct = |args: &[&str], kubeconfig: &Path| -> Output {
        let mut command = Command::new(kubectl);
        command
            .args(args)
            .env("HOME", &root)
            .env("KUBECONFIG", kubeconfig)
            .env("PATH", format!("{}/bin:/usr/bin:/bin", root.display()))
            .env("FIXTURE_ROOT", &root);
        run(&mut command, Duration::from_secs(10))
    };
    let kubeconfig = root.join("kubeconfig");
    let plugin_calls = || fs::read_to_string(root.join("plugin-calls")).unwrap();
    let native_calls = || recorded_argv(&root.join("native-calls"));
    for alias in ["ctx", "contexts", "ns", "pods"] {
        let alias_plugin = format!("kubectl-{alias}");
        install(&root, &alias_plugin, "create-plugin");
        for suffix in [&[][..], &["review-space"]] {
            for prefix in ["", "kubectl "] {
                let args = [&[alias][..], suffix].concat();
                let output = direct(&args, &kubeconfig);
                assert_eq!(output.returncode, 7, "{output:?}");
                let before = splitlines(&plugin_calls()).len();
                harness.send(format!(":{prefix}{}\r", args.join(" ")).as_bytes(), "Exit code 7");
                assert!(harness.text().contains(strip(&output.stdout())), "{}", harness.text());
                let text = plugin_calls();
                let calls = splitlines(&text);
                assert!(
                    calls.len() == before + 1
                        && serde_json::from_str::<Vec<String>>(calls[calls.len() - 1]).unwrap() == suffix,
                    "{calls:?}"
                );
                let native = native_calls();
                assert!(
                    native.last().is_some_and(|last| *last == args) && harness.text().contains("Plugin-defined target"),
                    "{native:?}"
                );
                harness.send(b"\r", "[Kubernetes]");
            }
        }
        fs::remove_file(root.join("bin").join(&alias_plugin)).unwrap();
        if alias == "ctx" || alias == "contexts" {
            harness.send(format!(":{alias}\r").as_bytes(), "k8s contexts list");
            harness.until("old-cluster");
        }
    }
    for prefix in ["", "kubectl "] {
        let args = ["create", "hamnfixture", "marker", "--plugin-option", "value"];
        let output = direct(&args, &kubeconfig);
        assert_eq!(output.returncode, 7, "{output:?}");
        let before = splitlines(&plugin_calls()).len();
        harness.send(format!(":{prefix}{}\r", args.join(" ")).as_bytes(), "Exit code 7");
        assert!(harness.text().contains(strip(&output.stdout())), "{}", harness.text());
        let text = plugin_calls();
        let calls = splitlines(&text);
        assert!(
            calls.len() == before + 1
                && serde_json::from_str::<Vec<String>>(calls[calls.len() - 1]).unwrap() == args[2..],
            "{calls:?}"
        );
        harness.send(b"\r", "[Kubernetes]");
    }

    for name in BUILTINS {
        let args = ["create", name, "--help"];
        let output = direct(&args, &kubeconfig);
        assert!(output.returncode == 0 && !output.stdout().contains("UNEXPECTED_SHADOW_PLUGIN"));
        harness.send(format!(":{}\r", args.join(" ")).as_bytes(), "Exit code 0");
        let screen = harness.text();
        assert!(!screen.contains("UNEXPECTED_SHADOW_PLUGIN") && !screen.contains("Plugin-defined"), "{screen}");
        harness.send(b"\r", "[Kubernetes]");
    }
    // The ordinary built-in still uses UI namespace defaults. This dry run
    // creates only stdout and cannot create a Kubernetes resource.
    harness.send(b":create configmap example --dry-run=client --validate=false -o yaml\r", "Exit code 0");
    assert!(harness.text().contains("namespace: ui-ns"), "{}", harness.text());
    harness.send(b"\r", "[Kubernetes]");
    let config_before = fs::read(&kubeconfig).unwrap();
    for value in ["--namespace=value", "--context=value", "--kubeconfig=value"] {
        for joined in [false, true] {
            let literal = format!("--from-literal={value}");
            let from_literal: Vec<&str> = if joined { vec![&literal] } else { vec!["--from-literal", value] };
            let args = [
                &["create", "configmap", "literal-fixture"][..],
                &from_literal,
                &["--dry-run=client", "--validate=false", "-o", "yaml"],
            ]
            .concat();
            let output =
                direct(&[&["--context", "old-cluster", "--namespace", "ui-ns"][..], &args].concat(), &kubeconfig);
            assert!(output.returncode == 0 && output.stdout().contains("namespace: ui-ns"), "{output:?}");
            let creates = || -> Vec<Vec<String>> {
                native_calls().into_iter().filter(|call| call.iter().any(|arg| arg == "literal-fixture")).collect()
            };
            let before = creates().len();
            harness.send(format!(":{}\r", args.join(" ")).as_bytes(), "Exit code 0");
            let screen = harness.text();
            let stdout = output.stdout();
            assert!(splitlines(&stdout).iter().all(|line| screen.contains(strip(line))), "{screen}");
            assert!(!screen.split("apiVersion:").next().unwrap().contains(value), "{screen}");
            let created = creates();
            assert!(
                created.len() == before + 1
                    && created[created.len() - 1]
                        .ends_with(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>()),
                "{created:?}"
            );
            harness.send(b"\r", "[Kubernetes]");
        }
    }
    assert_eq!(fs::read(&kubeconfig).unwrap(), config_before);
    for option in ["--dry-run", "--validate"] {
        let args = [
            "create",
            "configmap",
            "optional-fixture",
            option,
            "--namespace",
            "explicit",
            "--dry-run=client",
            "--validate=false",
            "-o",
            "yaml",
        ];
        let output = direct(&[&["--context", "old-cluster", "--namespace", "ui-ns"][..], &args].concat(), &kubeconfig);
        assert!(output.returncode == 0 && output.stdout().contains("namespace: explicit"), "{output:?}");
        harness.send(format!(":{}\r", args.join(" ")).as_bytes(), "Exit code 0");
        let screen = harness.text();
        assert!(splitlines(&output.stdout()).iter().all(|line| screen.contains(strip(line))), "{screen}");
        assert!(screen.split("apiVersion:").next().unwrap().contains("--namespace explicit"), "{screen}");
        harness.send(b"\r", "[Kubernetes]");
    }
    // A config value that resembles --kubeconfig must not redirect the
    // post-command reload to another file. Both edits own disposable files.
    let direct_config = root.join("direct-kubeconfig");
    fs::write(&direct_config, fs::read(&kubeconfig).unwrap()).unwrap();
    let args = [
        "config",
        "set-credentials",
        "literal-user",
        "--exec-command=/not-executed",
        "--exec-arg",
        "--kubeconfig=literal",
    ];
    let output = direct(&args, &direct_config);
    assert_eq!(output.returncode, 0, "{}", output.stderr());
    harness.send(format!(":{}\r", args.join(" ")).as_bytes(), "Exit code 0");
    assert!(harness.text().contains(strip(&output.stdout())), "{}", harness.text());
    harness.send(b"\r", "Namespace: test");
    assert!(harness.text().contains("Context: old-cluster"), "{}", harness.text());
    assert_eq!(fs::read(&kubeconfig).unwrap(), fs::read(&direct_config).unwrap());
    assert_hamn_directory_holds_preferences_only(&root);
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME"))
}

/// The `kubectl` wrapper: appends its argv to `$HOME/native-calls`, then
/// execs the installed kubectl.
pub fn kubectl_recorded(_program: &str, args: &[String]) -> ExitCode {
    real_cli::append_line(&home().join("native-calls"), &json_list(args));
    real_cli::exec(Command::new(real_cli::real("kubectl")).args(args))
}

/// `kubectl-create-hamnfixture` and the alias plugins: record the argv in
/// `$HOME/plugin-calls`, print it and exit 7.
pub fn plugin(_program: &str, args: &[String]) -> ExitCode {
    real_cli::append_line(&home().join("plugin-calls"), &json_list(args));
    println!("PLUGIN_ARGV={}", json_list(args));
    std::io::stdout().flush().unwrap();
    ExitCode::from(7)
}

/// A plugin shadowing `create` or a built-in `create` subcommand: must
/// never run.
pub fn shadow_plugin(_program: &str, _args: &[String]) -> ExitCode {
    print!("UNEXPECTED_SHADOW_PLUGIN");
    std::io::stdout().flush().unwrap();
    ExitCode::from(99)
}
