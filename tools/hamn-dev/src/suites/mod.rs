//! Host regression suites, each replacing one former tests/host script.
//! `hamn-dev test SUITE [FILTER...]` runs a suite's cases whose names
//! contain a filter (all cases without filters).
use std::process::ExitCode;

mod native_query_lifetime;
mod single_binary;
mod tui_backpressure;
mod tui_environment_actions;
mod tui_native_regressions;
mod tui_outcomes;
mod tui_picker_restore;
mod tui_quoting;
mod tui_reload;
mod tui_review_improvements;
mod tui_session_management;

type Suite = (&'static str, fn(&[String]) -> ExitCode);
type Fixture = fn(&str, &[String]) -> ExitCode;

const SUITES: &[Suite] = &[
    ("single-binary", single_binary::main),
    ("tui-native-regressions", tui_native_regressions::main),
    ("tui-review-improvements", tui_review_improvements::main),
    ("tui-session-management", tui_session_management::main),
    ("tui-outcomes", tui_outcomes::main),
    ("tui-quoting", tui_quoting::main),
    ("tui-reload", tui_reload::main),
    ("native-query-lifetime", native_query_lifetime::main),
    ("tui-environment-actions", tui_environment_actions::main),
    ("tui-picker-restore", tui_picker_restore::main),
    ("tui-backpressure", tui_backpressure::main),
];

const FIXTURES: &[(&str, Fixture)] = &[
    ("native-regressions", tui_native_regressions::fixture),
    ("tui-review-improvements", tui_review_improvements::peer),
    ("tui-review-improvements-compose", tui_review_improvements::compose_peer),
    ("tui-session-management-forward", tui_session_management::forward_peer),
    ("tui-session-management-timeout", tui_session_management::timeout_peer),
    ("tui-quoting-kubectl", tui_quoting::installed_kubectl),
    ("tui-reload", tui_reload::fixture),
    ("native-query-lifetime", native_query_lifetime::fixture),
    ("native-query-lifetime-child", native_query_lifetime::child),
    ("tui-picker-restore", tui_picker_restore::peer),
    ("tui-backpressure", tui_backpressure::fixture),
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
