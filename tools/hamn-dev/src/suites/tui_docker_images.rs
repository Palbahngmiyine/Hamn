//! Preserves the installed Docker CLI's image display options against an
//! owned Unix-socket Engine API: digests, full IDs and the tree view run as
//! the original argv in a terminal with the CLI's output and exit code, and
//! ordinary images remain a selectable list.
use crate::runner::{self, case};
use crate::support::docker_engine;
use crate::support::http::{Reply, Request, Server};
use crate::support::py_text::{self, splitlines, squash};
use crate::support::real_cli::{self, recorded_argv, run};
use crate::support::screen::RatatuiScreen;
use crate::support::tui::Harness;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let Some(docker) = real_cli::which("docker") else {
        println!("SKIP: installed Docker unavailable for image display comparison");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "tui-docker-images",
        "installed Docker digests/full IDs/tree retain original argv, output and exit code; \
         ordinary images remain selectable",
        vec![case("main", move || docker_images(&docker))],
        filters,
    )
}

fn image_id() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn digest() -> String {
    format!("sha256:{}", "b".repeat(64))
}

/// The Engine API with one image, `digest-fixture:latest`.
fn engine(socket: &Path) -> Server {
    docker_engine::serve(socket, |request: &Request| -> Reply {
        let (path, _) = py_text::urlsplit(&request.target);
        let body = if path.ends_with("/_ping") {
            b"OK".to_vec()
        } else if path.ends_with("/images/json") {
            json!([{"Id": image_id(), "RepoTags": ["digest-fixture:latest"],
                "RepoDigests": [format!("digest-fixture@{}", digest())], "Created": 0,
                "Size": 4096, "SharedSize": 0, "VirtualSize": 4096,
                "Labels": {}, "Containers": 0}])
            .to_string()
            .into_bytes()
        } else {
            return docker_engine::not_found(request);
        };
        docker_engine::reply(request, &body, None)
    })
}

fn docker_images(docker: &Path) {
    // Declared before the harness so that it stops after Hamn does.
    let _server;
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    let root = harness.root.clone();
    let endpoint = root.join("images.sock");
    _server = engine(&endpoint);
    real_cli::wrap(&root, "docker", docker, "docker-images-recorded");
    let target = ["--host".to_owned(), format!("unix://{}", endpoint.display())];
    // Keep complete CLI lines visible; resize mechanics have separate PTY tests.
    harness.screen = RatatuiScreen::new(32, 320);
    harness.pty.resize(32, 320);
    harness.send(format!(":docker {} images\r", target.join(" ")).as_bytes(), "digest-fixture");
    assert!(!harness.text().contains("docker terminal"));
    for command in [
        "images --digests",
        "image ls --digests",
        "image list --digests",
        "images --digests=false",
        "images --digests=true --digests=false",
        "images --digests=false --digests",
        "images --no-trunc",
        "images --digests --no-trunc",
        "images --tree",
    ] {
        let args: Vec<String> =
            target.iter().cloned().chain(py_text::split(command).into_iter().map(str::to_owned)).collect();
        let mut direct = Command::new(docker);
        real_cli::without_docker_variables(direct.args(&args).env("HOME", &root));
        let direct = run(&mut direct, Duration::from_secs(10));
        if !command.contains("--tree") {
            assert_eq!(direct.returncode, 0, "{}", direct.stderr());
        }
        let calls = || recorded_argv(&root.join("native-calls"));
        let before = calls().len();
        harness.send(format!(":docker {}\r", args.join(" ")).as_bytes(), &format!("Exit code {}", direct.returncode));
        // Process exit and PTY output arrive independently. Keep the full
        // output oracle, but allow the remaining output events to render.
        let output = direct.stdout() + &direct.stderr();
        let expected_lines: Vec<String> = splitlines(&output).into_iter().map(squash).collect();
        harness.wait(|harness| {
            let screen = squash(&harness.text());
            expected_lines.iter().all(|line| screen.contains(line.as_str()))
        });
        let screen = harness.text();
        // Returning from the preceding terminal refreshes the original list.
        let refresh: Vec<String> = target
            .iter()
            .cloned()
            .chain(["images", "--format", "{{json .}}", "--no-trunc"].map(str::to_owned))
            .collect();
        let new_calls: Vec<Vec<String>> = calls()[before..].iter().filter(|call| **call != refresh).cloned().collect();
        assert_eq!(new_calls, [args.clone()], "{:?}", &calls()[before..]);
        assert!(screen.contains("docker terminal"), "{screen}");
        for line in splitlines(&output) {
            assert!(squash(&screen).contains(&squash(line)), "{command} {line:?}\n{screen}");
        }
        if direct.stdout().contains(&digest()) {
            assert!(screen.contains(&digest()), "{screen}");
        }
        if command.contains("--no-trunc") {
            assert!(screen.contains(&image_id()), "{screen}");
        }
        harness.send(b"\r", "[Containers]");
        // A redraw may arrive in fragments. Do not let the previous exit
        // footer satisfy the next command's completion check.
        harness.wait(|harness| {
            let text = harness.text();
            !text.contains("docker terminal") && !text.contains("Exit code")
        });
        println!("PASS: {command} matches installed CLI output and exit {}", direct.returncode);
    }
}

/// The `docker` wrapper: appends its argv to `$HOME/native-calls`, then execs
/// the installed Docker without any inherited `DOCKER_*` variable and with
/// `DOCKER_API_VERSION=1.47`.
pub fn docker_images_recorded(_program: &str, args: &[String]) -> ExitCode {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    real_cli::append_line(&home.join("native-calls"), &py_text::json_list(args));
    real_cli::exec(&mut real_cli::docker_command(&real_cli::real("docker"), args))
}
