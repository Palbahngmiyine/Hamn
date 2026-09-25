//! The hosted Linux image builder removes `passt` so libguestfs keeps QEMU
//! SLIRP networking, and fails when removal is incomplete. The exact shell
//! lines of release.yml's "Install immutable image builder" step, from the
//! `# libguestfs auto-selects passt` comment up to the `multiarch=` line,
//! run under `bash -euo pipefail` with only a fixture `sudo` (this
//! executable) and an optional `passt` on PATH. Run from the repository
//! root.
use crate::runner::{self, case};
use crate::support::exec::output_within;
use crate::support::release_driver::outcome;
use crate::support::tmp::TempDir;
use serde_yaml::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-network",
        "hosted networking removes passt and rejects incomplete package removal",
        vec![
            case("absent_passt_needs_nothing_more", || scenario(&network_step(), Scenario::Absent, 0)),
            case("present_passt_is_purged", || scenario(&network_step(), Scenario::Present, 0)),
            case("purge_failure_stops_the_step", || scenario(&network_step(), Scenario::PurgeFailure, 42)),
            case("retained_executable_fails_the_step", || scenario(&network_step(), Scenario::RetainedExecutable, 1)),
            case("scenarios_detect_a_missing_post_purge_check", scenarios_detect_a_missing_post_purge_check),
        ],
        filters,
    )
}

const WORKFLOW: &str = ".github/workflows/release.yml";
const START: &str = "# libguestfs auto-selects passt";
const END: &str = "multiarch=$(dpkg-architecture";

/// The step's lines from the START comment to before the END line.
fn network_step() -> String {
    let text = fs::read_to_string(WORKFLOW).unwrap_or_else(|error| panic!("{WORKFLOW}: {error}"));
    let workflow: Value = serde_yaml::from_str(&text).unwrap();
    let steps = workflow["jobs"]["guest-image"]["steps"].as_sequence().expect("guest-image steps");
    let named: Vec<&Value> =
        steps.iter().filter(|step| step["name"].as_str() == Some("Install immutable image builder")).collect();
    let [step] = named.as_slice() else { panic!("expected one image builder install step, found {}", named.len()) };
    let run = step["run"].as_str().expect("the install step runs shell");
    let lines: Vec<&str> = run.lines().collect();
    let starts: Vec<usize> = (0..lines.len()).filter(|&index| lines[index].contains(START)).collect();
    let [start] = starts.as_slice() else { panic!("expected one {START:?} line, found {}", starts.len()) };
    let end = (start + 1..lines.len()).find(|&index| lines[index].contains(END)).expect("the multiarch line follows");
    let snippet = lines[*start..end].join("\n") + "\n";
    assert!(snippet.contains("sudo apt-get purge --yes passt"), "{snippet}");
    snippet
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    /// No passt; the purge succeeds.
    Absent,
    /// passt exists; the purge removes it.
    Present,
    /// The purge fails with status 42.
    PurgeFailure,
    /// The purge succeeds but leaves the executable.
    RetainedExecutable,
}

/// Runs `snippet` in `scenario`; its exit status must be `expected`.
fn scenario(snippet: &str, scenario: Scenario, expected: i32) {
    let work = TempDir::new("hamn-release-network-");
    let script = work.path().join("network.sh");
    fs::write(&script, snippet).unwrap();
    let bin = work.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let executable = std::env::current_exe().unwrap();
    std::os::unix::fs::symlink(&executable, bin.join("sudo")).unwrap();
    let passt: PathBuf = bin.join("passt");
    if !matches!(scenario, Scenario::Absent) {
        std::os::unix::fs::symlink(&executable, &passt).unwrap();
    }
    let purge = match scenario {
        Scenario::Absent | Scenario::Present => "remove",
        Scenario::PurgeFailure => "fail",
        Scenario::RetainedExecutable => "keep",
    };
    let mut command = Command::new("/bin/bash");
    command.args(["-euo", "pipefail"]).arg(&script).env_clear();
    command.env("PATH", &bin).env("HAMN_DEV_FIXTURE", "release-network-sudo");
    command.env("HAMN_TEST_PURGE", purge).env("HAMN_TEST_PASST", &passt);
    let outcome = outcome(output_within(&mut command, Duration::from_secs(60)));
    assert_eq!(outcome.code, Some(expected), "{scenario:?}: {outcome:#?}");
}

/// Without the check after the purge, a retained executable would pass,
/// so the matrix above is what catches that check's removal.
fn scenarios_detect_a_missing_post_purge_check() {
    let snippet = network_step();
    let check_start = snippet.find("if command -v passt").expect("the post-purge check");
    let check_end = check_start + snippet[check_start..].find("fi\n").expect("the check's end") + "fi\n".len();
    let weakened = format!("{}{}", &snippet[..check_start], &snippet[check_end..]);
    scenario(&weakened, Scenario::RetainedExecutable, 0);
    scenario(&weakened, Scenario::PurgeFailure, 42);
}

/// Fixture `sudo`: accepts only `apt-get purge --yes passt` (else 99), then
/// per `$HAMN_TEST_PURGE` removes `$HAMN_TEST_PASST` (`remove`), fails
/// with 42 (`fail`) or leaves it (`keep`).
pub fn sudo(_program: &str, args: &[String]) -> ExitCode {
    if args != ["apt-get", "purge", "--yes", "passt"] {
        return ExitCode::from(99);
    }
    match std::env::var("HAMN_TEST_PURGE").as_deref() {
        Ok("remove") => {
            let passt = std::env::var_os("HAMN_TEST_PASST").map(PathBuf::from).unwrap_or_default();
            match fs::remove_file(Path::new(&passt)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(99),
            }
        }
        Ok("fail") => ExitCode::from(42),
        Ok("keep") => ExitCode::SUCCESS,
        _ => ExitCode::from(99),
    }
}
