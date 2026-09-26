//! Hamn's development tooling: host publication, host regression suites and
//! release helpers. Nothing here is shipped in a release artifact.
//!
//! Invoked under another name (a `docker` or `kubectl` link in a test's
//! private `bin/`), the executable acts as that suite's recorded CLI fixture.
mod build_host;
mod fixture;
mod release;
mod runner;
mod suites;
// Shared by suites; not every helper is used by every build of them.
#[allow(dead_code)]
mod support;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_default();
    let name = std::path::Path::new(&program).file_name().and_then(|name| name.to_str()).unwrap_or("");
    if name != "hamn-dev" {
        return fixture::dispatch(name, args.collect());
    }
    let args: Vec<String> = args.collect();
    let result = match args.first().map(String::as_str) {
        Some("build-host") => build_host::run(&args[1..]),
        Some("check-host-binary") => match &args[1..] {
            [binary] => build_host::check_host_binary(binary.as_ref()),
            _ => Err("usage: hamn-dev check-host-binary BINARY".into()),
        },
        Some("release") => release::run(&args[1..]),
        Some("test") => return suites::run(&args[1..]),
        _ => Err(format!("usage: hamn-dev build-host|check-host-binary|release|test ...\n{}\n{}", release::usage(), suites::usage())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("hamn-dev: {message}");
            ExitCode::FAILURE
        }
    }
}
