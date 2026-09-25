//! Shared test support: PTYs, the screen recorder, temporary directories and
//! the TUI harness.
pub mod api_fixtures;
pub mod http;
pub mod pty;
pub mod screen;
pub mod tmp;
pub mod tui;

use std::path::PathBuf;

/// The Hamn executable under test: `$HAMN`, or build/hamn.
pub fn hamn() -> PathBuf {
    let path = std::env::var_os("HAMN").map_or_else(|| PathBuf::from("build/hamn"), PathBuf::from);
    std::fs::canonicalize(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}
