//! Host regression suites, each replacing one former tests/host script.
//! `hamn-dev test SUITE [FILTER...]` runs a suite's cases whose names
//! contain a filter (all cases without filters).
use std::process::ExitCode;

mod single_binary;
mod tui_cluster_target;
mod tui_docker_all;
mod tui_docker_images;
mod tui_kubectl_output;
mod tui_native_regressions;
mod tui_tls_target;

type Suite = (&'static str, fn(&[String]) -> ExitCode);
type Fixture = fn(&str, &[String]) -> ExitCode;

const SUITES: &[Suite] = &[
    ("single-binary", single_binary::main),
    ("tui-cluster-target", tui_cluster_target::main),
    ("tui-docker-all", tui_docker_all::main),
    ("tui-docker-images", tui_docker_images::main),
    ("tui-kubectl-output", tui_kubectl_output::main),
    ("tui-native-regressions", tui_native_regressions::main),
    ("tui-tls-target", tui_tls_target::main),
];

const FIXTURES: &[(&str, Fixture)] = &[
    ("native-regressions", tui_native_regressions::fixture),
    ("exec-real", crate::support::real_cli::exec_real),
    ("docker-api-1.47", tui_docker_all::docker_api_1_47),
    ("docker-images-recorded", tui_docker_images::docker_images_recorded),
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
