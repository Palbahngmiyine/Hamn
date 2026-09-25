//! Host regression suites, each replacing one former tests/host script.
//! `hamn-dev test SUITE [FILTER...]` runs a suite's cases whose names
//! contain a filter (all cases without filters).
use std::process::ExitCode;

mod control_signed_bootstrap;
mod core_worker;
mod docker_api;
mod docker_context;
mod docker_readiness;
mod exec_auth;
mod kubernetes_api;
mod native_flag_inventory;
mod native_query_lifetime;
mod observer_requests;
mod profile_yaml;
mod remote_cancel_boundaries;
mod rust_sdk;
mod single_binary;
mod ssh_deadline;
mod start_preflight;
mod tui;
mod tui_backpressure;
mod tui_cluster_target;
mod tui_create_plugins;
mod tui_docker_all;
mod tui_docker_config;
mod tui_docker_images;
mod tui_environment_actions;
mod tui_guarded_delete;
mod tui_kubectl_output;
mod tui_native_regressions;
mod tui_navigation;
mod tui_outcomes;
mod tui_picker_restore;
mod tui_quoting;
mod tui_reload;
mod tui_review_improvements;
mod tui_session_management;
mod tui_ssh_timeout;
mod tui_tls_target;
mod tui_workspaces;
mod udp_proxy;
mod update_check;
mod update_ux;
mod upgrade_cli;
mod upgrade_concurrency;
mod upgrade_native;
mod upgrade_properties;
mod upgrade_recovery_ownership;

type Suite = (&'static str, fn(&[String]) -> ExitCode);
type Fixture = fn(&str, &[String]) -> ExitCode;

const SUITES: &[Suite] = &[
    ("docker-readiness", docker_readiness::main),
    ("observer-requests", observer_requests::main),
    ("remote-cancel-boundaries", remote_cancel_boundaries::main),
    ("single-binary", single_binary::main),
    ("ssh-deadline", ssh_deadline::main),
    ("control-signed-bootstrap", control_signed_bootstrap::main),
    ("core-worker", core_worker::main),
    ("docker-api", docker_api::main),
    ("docker-context", docker_context::main),
    ("exec-auth", exec_auth::main),
    ("kubernetes-api", kubernetes_api::main),
    ("profile-yaml", profile_yaml::main),
    ("start-preflight", start_preflight::main),
    ("tui-native-regressions", tui_native_regressions::main),
    ("udp-proxy", udp_proxy::main),
    ("tui-review-improvements", tui_review_improvements::main),
    ("tui-session-management", tui_session_management::main),
    ("tui-outcomes", tui_outcomes::main),
    ("tui-quoting", tui_quoting::main),
    ("tui-reload", tui_reload::main),
    ("native-query-lifetime", native_query_lifetime::main),
    ("tui-environment-actions", tui_environment_actions::main),
    ("tui-picker-restore", tui_picker_restore::main),
    ("tui-backpressure", tui_backpressure::main),
    ("native-flag-inventory", native_flag_inventory::main),
    ("rust-sdk", rust_sdk::main),
    ("tui", tui::main),
    ("tui-guarded-delete", tui_guarded_delete::main),
    ("tui-navigation", tui_navigation::main),
    ("tui-ssh-timeout", tui_ssh_timeout::main),
    ("tui-workspaces", tui_workspaces::main),
    ("tui-cluster-target", tui_cluster_target::main),
    ("tui-create-plugins", tui_create_plugins::main),
    ("tui-docker-all", tui_docker_all::main),
    ("tui-docker-config", tui_docker_config::main),
    ("tui-docker-images", tui_docker_images::main),
    ("tui-kubectl-output", tui_kubectl_output::main),
    ("tui-tls-target", tui_tls_target::main),
    ("upgrade-cli", upgrade_cli::main),
    ("upgrade-concurrency", upgrade_concurrency::main),
    ("upgrade-native", upgrade_native::main),
    ("upgrade-properties", upgrade_properties::main),
    ("upgrade-recovery-ownership", upgrade_recovery_ownership::main),
    ("update-check", update_check::main),
    ("update-ux", update_ux::main),
];

const FIXTURES: &[(&str, Fixture)] = &[
    ("control-signed-bootstrap", control_signed_bootstrap::updater_fixture),
    ("core-worker-external-cli", core_worker::external_cli_fixture),
    ("exec-auth", exec_auth::fixture),
    ("native-regressions", tui_native_regressions::fixture),
    (ssh_deadline::UNRESPONSIVE_SSH, ssh_deadline::unresponsive_ssh),
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
    ("rust-sdk", rust_sdk::fixture),
    ("tui-guarded-delete", tui_guarded_delete::fixture),
    ("tui-workspaces", tui_workspaces::fixture),
    ("exec-real", crate::support::real_cli::exec_real),
    ("docker-api-1.47", tui_docker_all::docker_api_1_47),
    ("docker-images-recorded", tui_docker_images::docker_images_recorded),
    ("docker-config-root", tui_docker_config::docker_config_root),
    ("create-plugins-kubectl", tui_create_plugins::kubectl_recorded),
    ("create-plugin", tui_create_plugins::plugin),
    ("create-shadow-plugin", tui_create_plugins::shadow_plugin),
    ("update-check-cli", update_check::cli_fixture),
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
