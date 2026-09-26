//! The published executable is one signed Mach-O file that still works
//! after it is copied elsewhere.
use crate::build_host::check_host_binary;
use crate::runner::{self, case};
use crate::support::{hamn, tmp::TempDir};
use std::process::{Command, ExitCode, Stdio};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "single-binary",
        "single binary relocation",
        vec![case("relocated_copy_reports_the_same_version", relocated_copy_reports_the_same_version)],
        filters,
    )
}

fn relocated_copy_reports_the_same_version() {
    let binary = hamn();
    check_host_binary(&binary).unwrap();
    let temporary = TempDir::new("hamn-single-binary-");
    let copy = temporary.path().join("hamn");
    std::fs::copy(&binary, &copy).unwrap();
    let version = |command: &mut Command| {
        let output = command.arg("--version").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        output.stdout
    };
    assert_eq!(version(&mut Command::new(&binary)), version(Command::new("./hamn").current_dir(temporary.path())));
    let status = Command::new(&copy).arg("--help").stdout(Stdio::null()).status().unwrap();
    assert!(status.success());
}
