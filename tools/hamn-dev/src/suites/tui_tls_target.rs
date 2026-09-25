//! Uses the installed kubectl and a disposable HTTPS API to verify that the
//! menu keeps TLS targeting: the list and the selected inspect both retain
//! the certificate-name override.
use crate::runner::{self, case};
use crate::support::http::{Options, Server};
use crate::support::kube_api;
use crate::support::real_cli::{self, run_checked};
use crate::support::tui::Harness;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let Some(kubectl) = real_cli::which("kubectl") else {
        println!("SKIP: installed kubectl unavailable for HTTPS integration; argv regression is in Rust");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "tui-tls-target",
        "installed kubectl list and selected inspect retain TLS certificate-name override",
        vec![case("main", move || tls_target(&kubectl))],
        filters,
    )
}

fn tls_target(kubectl: &Path) {
    // Declared first so that it stops after Hamn does, as in the Python test.
    let server;
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    let root = harness.root.clone();
    let (cert, key) = review_certificate(&root);
    server = Server::tcp(Options { tls: Some((cert, key)), ..Options::default() }, kube_api::handle);
    real_cli::wrap(&root, "kubectl", kubectl, "exec-real");
    let query = format!(
        "kubectl get pods --server https://127.0.0.1:{} --certificate-authority {}/cert \
         --tls-server-name api.review.internal --token fixture",
        server.port(),
        root.display()
    );
    harness.send(format!(":{query}\r").as_bytes(), "tls-pod");
    harness.send(b"\r", "Exit code 0");
    assert!(harness.text().contains("name: tls-pod"), "{}", harness.text());
}

/// A self-signed CA certificate named only `api.review.internal`, made by
/// the `openssl` on PATH within 30 seconds. Returns the certificate and key.
fn review_certificate(root: &Path) -> (PathBuf, PathBuf) {
    let config = root.join("openssl.cnf");
    std::fs::write(
        &config,
        "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n\
         [dn]\nCN=api.review.internal\n[ext]\n\
         subjectAltName=DNS:api.review.internal\nbasicConstraints=CA:TRUE\n",
    )
    .unwrap();
    let (cert, key) = (root.join("cert"), root.join("key"));
    let mut command = Command::new("openssl");
    command.args(["req", "-x509", "-nodes", "-newkey", "rsa:2048", "-keyout"]).arg(&key).arg("-out").arg(&cert);
    command.args(["-days", "1", "-config"]).arg(&config);
    run_checked(&mut command, Duration::from_secs(30));
    (cert, key)
}
