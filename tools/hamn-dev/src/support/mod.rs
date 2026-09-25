//! Shared test support: PTYs, the screen recorder, temporary directories and
//! the TUI harness.
pub mod api_fixtures;
pub mod bounded_process;
pub mod c_extract;
pub mod docker_engine;
pub mod exec;
pub mod harness_peers;
pub mod http;
pub mod kube_api;
pub mod pty;
pub mod py_http;
pub mod py_text;
pub mod real_cli;
pub mod release_driver;
pub mod screen;
pub mod termios;
pub mod tmp;
pub mod tui;
pub mod upgrade;

use std::path::PathBuf;

/// The Hamn executable under test: `$HAMN`, or build/hamn.
pub fn hamn() -> PathBuf {
    let path = std::env::var_os("HAMN").map_or_else(|| PathBuf::from("build/hamn"), PathBuf::from);
    std::fs::canonicalize(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}
