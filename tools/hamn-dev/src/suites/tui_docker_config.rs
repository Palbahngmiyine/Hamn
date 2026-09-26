//! A context reload must keep the installed Docker CLI's effective config
//! directory: after `docker --config ... context show`, the list comes from
//! the endpoint of the same config the direct CLI uses (the last `--config`,
//! resolved relative to the CLI's working directory).
use crate::runner::{self, case};
use crate::support::docker_engine;
use crate::support::http::{Reply, Request, Server};
use crate::support::py_text::shlex_join;
use crate::support::real_cli::{self, run_checked};
use crate::support::tui::Harness;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let Some(docker) = real_cli::which("docker") else {
        println!("SKIP: installed Docker unavailable; config reload parsing is covered in Rust");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "tui-docker-config",
        "context reload retains the endpoint of installed Docker's effective config",
        vec![case("main", move || docker_config(&docker))],
        filters,
    )
}

/// The Engine API of one config's context: records `label` for each
/// container list and answers with `<label>-endpoint-row`.
fn engine(socket: &Path, label: &'static str, requests: Arc<Mutex<Vec<&'static str>>>) -> Server {
    docker_engine::serve(socket, move |request: &Request| -> Reply {
        let body = if request.target.ends_with("/_ping") {
            b"OK".to_vec()
        } else if request.target.contains("/containers/json") {
            requests.lock().unwrap().push(label);
            json!([{"Id": "a".repeat(64), "Names": [format!("/{label}-endpoint-row")],
                "Image": "fixture", "State": "running", "Status": "Up", "Ports": [],
                "Labels": {}, "Created": 0}])
            .to_string()
            .into_bytes()
        } else {
            return docker_engine::not_found(request);
        };
        docker_engine::reply(request, &body, Some("application/json"))
    })
}

fn docker_config(docker: &Path) {
    let requests: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    // Declared before the harness so that they stop after Hamn does.
    let mut servers: Vec<Server> = Vec::new();
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    let root = harness.root.clone();
    let docker_command = || {
        let mut command = Command::new(docker);
        real_cli::without_docker_variables(command.env("HOME", &root));
        command
    };
    for label in ["first", "last", "context"] {
        let config = root.join(label);
        fs::create_dir(&config).unwrap();
        let endpoint = config.join("api.sock");
        servers.push(engine(&endpoint, label, Arc::clone(&requests)));
        let host = format!("host=unix://{}", endpoint.display());
        for command in [&["create", "shared", "--docker", &host][..], &["use", "shared"]] {
            let mut context = docker_command();
            context.arg("--config").arg(&config).arg("context").args(command);
            run_checked(&mut context, Duration::from_secs(10));
        }
    }
    real_cli::wrap(&root, "docker", docker, "docker-config-root");
    let path = |label: &str| root.join(label).display().to_string();
    for (flags, label) in [
        (vec!["--config".to_owned(), path("first"), "--config".to_owned(), path("last")], "last"),
        (vec!["--config".to_owned(), "context".to_owned()], "context"),
        (vec!["--config".to_owned(), path("last"), format!("--config={}", path("first"))], "first"),
    ] {
        let mut direct = docker_command();
        direct.args(&flags).args(["ps", "--format", "{{json .}}"]).current_dir(&root);
        let direct = run_checked(&mut direct, Duration::from_secs(10));
        let row = format!("{label}-endpoint-row");
        assert!(direct.stdout().contains(&row), "{}", direct.stdout());
        let command: Vec<&str> =
            ["docker"].into_iter().chain(flags.iter().map(String::as_str)).chain(["context", "show"]).collect();
        harness.send(format!(":{}\r", shlex_join(&command)).as_bytes(), "Exit code 0");
        harness.send(b"\r", &row);
        assert_eq!(requests.lock().unwrap().last(), Some(&label), "{:?}", requests.lock().unwrap());
        println!("PASS: context reload retains {label} endpoint like installed Docker");
    }
}

/// The `docker` wrapper: execs the installed Docker from the fixture root
/// without any inherited `DOCKER_*` variable and with
/// `DOCKER_API_VERSION=1.47`, so a relative `--config` resolves there.
pub fn docker_config_root(_program: &str, args: &[String]) -> ExitCode {
    let root = std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT");
    real_cli::exec(real_cli::docker_command(&real_cli::real("docker"), args).current_dir(root))
}
