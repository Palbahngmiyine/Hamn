//! Double-quoted templates must reach the installed kubectl unchanged: the
//! TUI's output for a quoted command line matches a direct argv execution.
use crate::runner::{self, case};
use crate::support::harness_peers::{run_bounded, select_peer, which};
use crate::support::tui::Harness;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let Some(kubectl) = which("kubectl") else {
        println!("SKIP: installed kubectl unavailable; quoting has Rust regressions");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "tui-quoting",
        "real kubectl template and literal bytes match direct argv execution",
        vec![case("main", move || quoted_template_matches_direct_argv(&kubectl))],
        filters,
    )
}

fn quoted_template_matches_direct_argv(kubectl: &PathBuf) {
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    // bin/kubectl now execs the installed kubectl named in this file.
    std::fs::write(harness.root.join("installed-kubectl"), kubectl.as_os_str().as_bytes()).unwrap();
    select_peer(&harness.root, "kubectl", "tui-quoting-kubectl");
    let args = [
        "create",
        "configmap",
        "quoted",
        r"--from-literal=pattern=a\nb\tc\.d",
        "--dry-run=client",
        "--validate=false",
        "-o",
        r"jsonpath={.data.pattern}{'\n'}QUOTE_END",
    ];
    let direct = run_bounded(
        Command::new(kubectl).args(args).env("HOME", &harness.root).env("KUBECONFIG", harness.root.join("kubeconfig")),
        Duration::from_secs(15),
    );
    assert!(direct.status.success(), "{}", String::from_utf8_lossy(&direct.stderr));
    let stdout = String::from_utf8(direct.stdout).unwrap();
    assert_eq!(stdout, "a\\nb\\tc\\.d\nQUOTE_END", "{stdout}");
    let command = concat!(
        r#"kubectl create configmap quoted --from-literal=pattern="a\nb\tc\.d" "#,
        r#"--dry-run=client --validate=false -o "jsonpath={.data.pattern}{'\n'}QUOTE_END""#
    );
    harness.send(format!(":{command}\r").as_bytes(), "Exit code 0");
    let screen = harness.text();
    for line in stdout.lines() {
        assert!(screen.contains(line), "{line:?}\n{screen}");
    }
}

/// `kubectl` that execs the installed kubectl named in `installed-kubectl`.
pub fn installed_kubectl(_program: &str, args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    let kubectl = PathBuf::from(OsStr::from_bytes(&std::fs::read(root.join("installed-kubectl")).unwrap()));
    let error = Command::new(&kubectl).args(args).exec();
    panic!("exec {}: {error}", kubectl.display());
}
