//! Real updater transactions serialize on the install root locks without
//! rebuilding the shared executable. Version wrappers delegate every private
//! operation to a frozen copy of the Hamn under test; the native installer,
//! updater, receipt and journal run in an owned HOME, and the updater's
//! BEFORE_LOCK test barrier binds the assertions to lock acquisition.
use crate::runner::{self, case};
use crate::support::upgrade::{self, Releases, await_ready, mkfifo, ready_fifo, release};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "upgrade-concurrency",
        "queued updaters re-check the active generation and version under the root locks",
        vec![
            case("queued_frontend_cannot_replace_newer_active_generation", queued_frontend_cannot_replace_newer_active_generation),
            case("queued_bootstrap_becomes_an_update_and_rejects_downgrade", queued_bootstrap_becomes_an_update_and_rejects_downgrade),
            case("active_version_overrides_stale_frontend_version", active_version_overrides_stale_frontend_version),
            case(
                "writable_runtime_root_rejects_update_without_changing_installation",
                writable_runtime_root_rejects_update_without_changing_installation,
            ),
        ],
        filters,
    )
}

const WAIT: Duration = Duration::from_secs(30);

/// Starts the first updater and holds it at its prepared (journaled)
/// boundary, starts the second until it reaches the root-lock boundary, then
/// releases the first. The first must install 1.0.3; the second must fail
/// without changing the result. Returns the second updater's stderr.
fn queued_pair(fixture: &Releases, first: (&Path, &Path, &[&str]), second: (&Path, &Path, &[&str])) -> String {
    let (first_ready_path, first_ready) = ready_fifo(&fixture.root, "first-ready");
    let (second_ready_path, second_ready) = ready_fifo(&fixture.root, "second-ready");
    let release_first = fixture.root.join("release-first");
    mkfifo(&release_first);
    let mut first_child = fixture.spawn(
        first.0,
        first.1,
        first.2,
        &[("HAMN_TEST_UPDATE_PREPARED_READY_FIFO", &first_ready_path), ("HAMN_TEST_UPDATE_PREPARED_RELEASE_FIFO", &release_first)],
    );
    await_ready(&first_ready, Duration::from_secs(20), "first updater");
    let mut second_child =
        fixture.spawn(second.0, second.1, second.2, &[("HAMN_TEST_UPDATE_BEFORE_LOCK_READY_FIFO", &second_ready_path)]);
    await_ready(&second_ready, Duration::from_secs(20), "second updater");
    // The second invocation reached the root-lock boundary while the first
    // still owns the durable transaction lock (an announce-only barrier).
    assert!(second_child.running(), "the second updater did not wait for the lock owner");
    assert!(first_child.running());
    release(&release_first, WAIT);
    let first_result = first_child.finish(WAIT);
    assert_eq!(first_result.returncode, 0, "{}", first_result.stderr());
    let value: serde_json::Value = serde_json::from_slice(&first_result.stdout).unwrap();
    assert_eq!(value["latestVersion"], "1.0.3", "{value}");
    let active = fixture.active();
    let selected = std::fs::read(&fixture.selection).unwrap();
    let second_result = second_child.finish(WAIT);
    assert_ne!(second_result.returncode, 0, "{} {}", second_result.stdout(), second_result.stderr());
    assert_eq!(fixture.active(), active);
    assert_eq!(std::fs::read(&fixture.selection).unwrap(), selected);
    let version = upgrade::run(Command::new(&fixture.command).arg("--version"), Duration::from_secs(10));
    assert_eq!(version.stdout().trim(), "hamn 1.0.3");
    assert!(!fixture.journal().exists());
    second_result.stderr()
}

fn installed(fixture: &Releases, version: &str) -> PathBuf {
    let (script, manifest) = fixture.release(version);
    fixture.run_update(&script, &manifest, &["--bootstrap"]);
    fixture.installed()
}

fn queued_frontend_cannot_replace_newer_active_generation() {
    let fixture = Releases::new("hamn-upgrade-concurrency-");
    let invoked = installed(&fixture, "1.0.1");
    let (_, newer) = fixture.release("1.0.3");
    let (_, older) = fixture.release("1.0.2");
    let generation = invoked.to_str().unwrap().to_owned();
    let stale = ["--current-version", "1.0.1", "--generation", generation.as_str()];
    let error = queued_pair(&fixture, (&invoked, &newer, &stale), (&invoked, &older, &stale));
    assert!(error.contains("managed generation changed"), "{error}");
}

fn queued_bootstrap_becomes_an_update_and_rejects_downgrade() {
    let fixture = Releases::new("hamn-upgrade-concurrency-");
    let (first_script, newer) = fixture.release("1.0.3");
    let (second_script, older) = fixture.release("1.0.2");
    let error = queued_pair(&fixture, (&first_script, &newer, &["--bootstrap"]), (&second_script, &older, &["--bootstrap"]));
    assert!(error.contains("stable downgrade is not permitted"), "{error}");
}

fn active_version_overrides_stale_frontend_version() {
    let fixture = Releases::new("hamn-upgrade-concurrency-");
    let invoked = installed(&fixture, "1.0.3");
    let (_, older) = fixture.release("1.0.2");
    let active = fixture.active();
    let selection = std::fs::read(&fixture.selection).unwrap();
    let result =
        upgrade::run(&mut fixture.frontend(&invoked, &older, "1.0.1"), WAIT);
    assert_ne!(result.returncode, 0, "{}", result.stdout());
    assert!(result.stderr().contains("stable downgrade is not permitted"), "{}", result.stderr());
    assert_eq!(fixture.active(), active);
    assert_eq!(std::fs::read(&fixture.selection).unwrap(), selection);
}

fn writable_runtime_root_rejects_update_without_changing_installation() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Releases::new("hamn-upgrade-concurrency-");
    let invoked = installed(&fixture, "1.0.1");
    let (_, newer) = fixture.release("1.0.3");
    let (active, selected) = (fixture.active(), std::fs::read(&fixture.selection).unwrap());
    std::fs::set_permissions(fixture.home.join(".hamn"), std::fs::Permissions::from_mode(0o777)).unwrap();
    let result =
        upgrade::run(&mut fixture.frontend(&invoked, &newer, "1.0.1"), WAIT);
    assert_ne!(result.returncode, 0, "{}", result.stdout());
    assert!(result.stderr().contains("unsafe Hamn runtime root"), "{}", result.stderr());
    assert_eq!(fixture.active(), active);
    assert_eq!(std::fs::read(&fixture.selection).unwrap(), selected);
    assert!(!fixture.journal().exists());
}
