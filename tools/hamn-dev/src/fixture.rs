//! Fixture dispatch: this executable linked as `docker`, `kubectl` or a
//! plugin inside a test's private `bin/` acts as a recorded CLI peer. The
//! behavior comes from `$FIXTURE_ROOT/fixture-<name>` when present, else
//! from `HAMN_DEV_FIXTURE`, which the test harness sets.
use std::path::PathBuf;
use std::process::ExitCode;

pub fn dispatch(program: &str, args: Vec<String>) -> ExitCode {
    let root = std::env::var_os("FIXTURE_ROOT").map(PathBuf::from);
    let selected = root
        .and_then(|root| std::fs::read_to_string(root.join(format!("fixture-{program}"))).ok())
        .or_else(|| std::env::var("HAMN_DEV_FIXTURE").ok());
    let Some(name) = selected.as_deref().map(str::trim) else {
        eprintln!("hamn-dev: {program} is not a selected fixture");
        return ExitCode::from(127);
    };
    match crate::suites::fixture(name) {
        Some(fixture) => fixture(program, &args),
        None => {
            eprintln!("hamn-dev: unknown fixture {name:?} for {program}");
            ExitCode::from(127)
        }
    }
}
