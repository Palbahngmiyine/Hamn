//! Host regression suites, each replacing one former tests/host script.
//! `hamn-dev test SUITE [FILTER...]` runs a suite's cases whose names
//! contain a filter (all cases without filters).
use std::process::ExitCode;

mod native_flag_inventory;
mod rust_sdk;
mod single_binary;
mod tui;
mod tui_guarded_delete;
mod tui_native_regressions;
mod tui_navigation;
mod tui_ssh_timeout;
mod tui_workspaces;

type Suite = (&'static str, fn(&[String]) -> ExitCode);
type Fixture = fn(&str, &[String]) -> ExitCode;

const SUITES: &[Suite] = &[
    ("native-flag-inventory", native_flag_inventory::main),
    ("rust-sdk", rust_sdk::main),
    ("single-binary", single_binary::main),
    ("tui", tui::main),
    ("tui-guarded-delete", tui_guarded_delete::main),
    ("tui-native-regressions", tui_native_regressions::main),
    ("tui-navigation", tui_navigation::main),
    ("tui-ssh-timeout", tui_ssh_timeout::main),
    ("tui-workspaces", tui_workspaces::main),
];

const FIXTURES: &[(&str, Fixture)] = &[
    ("native-regressions", tui_native_regressions::fixture),
    ("rust-sdk", rust_sdk::fixture),
    ("tui-guarded-delete", tui_guarded_delete::fixture),
    ("tui-workspaces", tui_workspaces::fixture),
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
