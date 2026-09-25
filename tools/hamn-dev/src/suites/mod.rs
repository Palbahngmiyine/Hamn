//! Host regression suites, each replacing one former tests/host script.
//! `hamn-dev test SUITE [FILTER...]` runs a suite's cases whose names
//! contain a filter (all cases without filters).
use std::process::ExitCode;

mod docker_readiness;
mod observer_requests;
mod remote_cancel_boundaries;
mod single_binary;
mod ssh_deadline;
mod tui_native_regressions;

type Suite = (&'static str, fn(&[String]) -> ExitCode);
type Fixture = fn(&str, &[String]) -> ExitCode;

const SUITES: &[Suite] = &[
    ("docker-readiness", docker_readiness::main),
    ("observer-requests", observer_requests::main),
    ("remote-cancel-boundaries", remote_cancel_boundaries::main),
    ("single-binary", single_binary::main),
    ("ssh-deadline", ssh_deadline::main),
    ("tui-native-regressions", tui_native_regressions::main),
];

const FIXTURES: &[(&str, Fixture)] = &[
    ("native-regressions", tui_native_regressions::fixture),
    (ssh_deadline::UNRESPONSIVE_SSH, ssh_deadline::unresponsive_ssh),
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
