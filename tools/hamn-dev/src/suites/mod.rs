//! Host regression suites, each replacing one former tests/host script.
//! `hamn-dev test SUITE [FILTER...]` runs a suite's cases whose names
//! contain a filter (all cases without filters).
use std::process::ExitCode;

mod core_worker;
mod docker_api;
mod docker_context;
mod exec_auth;
mod kubernetes_api;
mod single_binary;
mod start_preflight;
mod tui_native_regressions;

type Suite = (&'static str, fn(&[String]) -> ExitCode);
type Fixture = fn(&str, &[String]) -> ExitCode;

const SUITES: &[Suite] = &[
    ("core-worker", core_worker::main),
    ("docker-api", docker_api::main),
    ("docker-context", docker_context::main),
    ("exec-auth", exec_auth::main),
    ("kubernetes-api", kubernetes_api::main),
    ("single-binary", single_binary::main),
    ("start-preflight", start_preflight::main),
    ("tui-native-regressions", tui_native_regressions::main),
];

const FIXTURES: &[(&str, Fixture)] = &[
    ("core-worker-external-cli", core_worker::external_cli_fixture),
    ("exec-auth", exec_auth::fixture),
    ("native-regressions", tui_native_regressions::fixture),
];

pub fn run(args: &[String]) -> ExitCode {
    let Some((name, filters)) = args.split_first() else {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };
    match SUITES.iter().find(|(suite, _)| suite == name) {
        Some((_, main)) => main(filters),
        None => {
            eprintln!("unknown suite {name:?}\n{}", usage());
            ExitCode::from(2)
        }
    }
}

pub fn fixture(name: &str) -> Option<Fixture> {
    FIXTURES.iter().find(|(fixture, _)| *fixture == name).map(|(_, fixture)| *fixture)
}

pub fn usage() -> String {
    let names: Vec<&str> = SUITES.iter().map(|(name, _)| *name).collect();
    format!("suites: {}", names.join(" "))
}
