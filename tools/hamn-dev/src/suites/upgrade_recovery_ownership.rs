//! A pending transaction may restore only the generation it actually
//! published. The native installer and updater of version-wrapped releases
//! and a frozen copy of the Hamn under test run in owned homes; test
//! barriers, the updater's fault seam and process-group SIGKILL make
//! interruptions reproducible. No VM, release network or shared build is
//! involved.
use crate::runner::{self, case};
use crate::support::upgrade::{self, Barrier, Releases, digest, file_digest};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "upgrade-recovery-ownership",
        "recovery restores only the generation its own transaction recorded",
        vec![
            case("later_home_host_update_survives_prior_host_transaction", || {
                later_home_survives_old_recovery(true)
            }),
            case("later_home_host_update_survives_prior_selection_only_transaction", || {
                later_home_survives_old_recovery(false)
            }),
            case("target_is_durable_before_link_publication_and_recovers_after_kill", || publication_interruption(false)),
            case("first_bootstrap_recovers_recorded_target_before_link_publication", || publication_interruption(true)),
            case("recovery_failure_keeps_exact_target_for_next_retry", recovery_failure_keeps_exact_target_for_next_retry),
            case("installer_rejects_foreign_journal_without_writing_it", installer_rejects_foreign_journal_without_writing_it),
            case("journals_without_an_attempted_target_are_refused_unchanged", journals_without_an_attempted_target_are_refused_unchanged),
        ],
        filters,
    )
}

const WAIT: Duration = Duration::from_secs(30);

/// Every path below `directory`: its mode, modification time and, for a
/// regular file, its digest.
type Snapshot = BTreeMap<PathBuf, (u32, i64, i64, Option<String>)>;

fn snapshot(directory: &Path) -> Snapshot {
    let mut result = Snapshot::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())) {
            let path = entry.unwrap().path();
            let info = fs::symlink_metadata(&path).unwrap();
            let content = info.is_file().then(|| file_digest(&path));
            if info.is_dir() {
                pending.push(path.clone());
            }
            let relative = path.strip_prefix(directory).unwrap().to_path_buf();
            result.insert(relative, (info.mode(), info.mtime(), info.mtime_nsec(), content));
        }
    }
    result
}

/// Holds an installed-generation update at barrier `point`, then SIGKILLs
/// its process group. Returns the journal the killed update left.
fn interrupt(fixture: &Releases, manifest: &Path, point: &str) -> PathBuf {
    let barrier = Barrier::new(&fixture.root, point, "interrupt");
    let mut command = fixture.updater(&fixture.installed(), manifest, &[]);
    let child = upgrade::Group::spawn(barrier.apply(&mut command));
    barrier.await_ready(Duration::from_secs(20), point);
    child.signal_group(libc::SIGKILL);
    let result = child.finish(Duration::from_secs(5));
    assert_eq!(result.returncode, -libc::SIGKILL, "{}", result.stderr());
    fixture.journal()
}

/// Runs an updater (the installed one by default) with a malformed
/// manifest: it recovers any pending transaction first, then fails.
fn recover(fixture: &Releases, program: Option<&Path>, bootstrap: bool, env: &[(&str, &str)]) -> upgrade::Output {
    let invalid = fixture.root.join("invalid.json");
    fs::write(&invalid, "{").unwrap();
    let program = program.map_or_else(|| fixture.installed(), Path::to_path_buf);
    let options: &[&str] = if bootstrap { &["--bootstrap"] } else { &[] };
    let mut command = fixture.updater(&program, &invalid, options);
    command.envs(env.iter().copied());
    upgrade::run(&mut command, WAIT)
}

fn later_home_survives_old_recovery(host_mutation: bool) {
    let fixture = Releases::new("hamn-recovery-ownership-");
    let (script, initial) = fixture.release("1.0.1");
    fixture.run_update(&script, &initial, &["--bootstrap"]);
    let (_, mut pending) = fixture.release("1.0.2");
    let (_, later) = fixture.release("1.0.3");
    if !host_mutation {
        fs::remove_file(&fixture.selection).unwrap();
        pending = initial;
    }
    let journal = interrupt(&fixture, &pending, "AFTER_GUEST_SELECTION");
    let state = fs::read_to_string(journal.join("state")).unwrap();
    assert!(state.contains(&format!("hostMutation={}", u8::from(host_mutation))), "{state}");
    let cache = fixture.selection.parent().unwrap().to_path_buf();
    let before_a = snapshot(&cache);
    let other = fixture.root.join("other-home");
    fs::create_dir(&other).unwrap();
    fs::set_permissions(&other, fs::Permissions::from_mode(0o700)).unwrap();
    let from_other = |manifest: &Path| {
        let mut command = fixture.updater(&fixture.installed(), manifest, &[]);
        command.env("HOME", &other);
        let result = upgrade::run(&mut command, WAIT);
        assert_eq!(result.returncode, 0, "{}", result.stderr());
    };
    from_other(&later);
    if host_mutation {
        // Move beyond the immediate predecessor, so preservation depends on
        // the attempted generation's pending recovery-root reference.
        let (_, newest) = fixture.release("1.0.4");
        from_other(&newest);
        let attempted = fs::read_to_string(journal.join("new-target")).unwrap();
        assert!(Path::new(attempted.trim_end()).is_file(), "{attempted}");
    }
    let active = fixture.active();
    let before_b = snapshot(&other.join(".hamn/cache"));
    assert_eq!(snapshot(&cache), before_a);
    let result = recover(&fixture, None, false, &[]);
    assert_ne!(result.returncode, 0);
    assert_eq!(fixture.active(), active, "a stale HOME journal rolled back a successful later generation");
    assert_eq!(snapshot(&cache), before_a, "ambiguous recovery changed its selection/cache or retired its journal");
    assert_eq!(snapshot(&other.join(".hamn/cache")), before_b);
    let stderr = result.stderr();
    for expected in ["does not own the active generation", "manual review", "retrying alone will not resolve"] {
        assert!(stderr.contains(expected), "{expected}: {stderr}");
    }
    assert!(journal.is_dir());
}

fn publication_interruption(bootstrap: bool) {
    let fixture = Releases::new("hamn-recovery-ownership-");
    let (mut original, mut selected) = (None, None);
    if !bootstrap {
        let (script, initial) = fixture.release("1.0.1");
        fixture.run_update(&script, &initial, &["--bootstrap"]);
        original = Some(fixture.active());
        selected = Some(fs::read(&fixture.selection).unwrap());
    }
    let (script, pending) = fixture.release("1.0.2");
    // Hold the installer right before it publishes the command link, after
    // it recorded the attempted target.
    let barrier = Barrier::new(&fixture.root, "BEFORE_LINK_PUBLICATION", "publication");
    let invoked = if bootstrap { script.clone() } else { fixture.installed() };
    let options: &[&str] = if bootstrap { &["--bootstrap"] } else { &[] };
    let mut command = fixture.updater(&invoked, &pending, options);
    let child = upgrade::Group::spawn(barrier.apply(&mut command));
    barrier.await_ready(Duration::from_secs(20), "installer publication");
    let journal = fixture.journal();
    let attempted = PathBuf::from(fs::read_to_string(journal.join("new-target")).unwrap().trim_end());
    assert_ne!(Some(&attempted), original.as_ref());
    assert!(attempted.is_file());
    let link = || fs::read_link(&fixture.command).ok();
    assert_eq!(link(), original);
    let cache = fs::canonicalize(fixture.selection.parent().unwrap()).unwrap();
    let cache_text = cache.to_str().unwrap();
    let generation = attempted.parent().and_then(Path::parent).unwrap();
    let root_record = generation.join(format!(".hamn-recovery-root-{}", digest(cache_text.as_bytes())));
    assert_eq!(fs::read_to_string(root_record).unwrap(), cache_text);
    child.signal_group(libc::SIGKILL);
    let killed = child.finish(Duration::from_secs(5));
    assert_eq!(killed.returncode, -libc::SIGKILL);
    let result = recover(&fixture, Some(&invoked), bootstrap, &[]);
    assert_ne!(result.returncode, 0);
    let expected = if bootstrap { "Recovered interrupted bootstrap" } else { "recovered the previous binary" };
    assert!(result.stderr().contains(expected), "{}", result.stderr());
    assert_eq!(link(), original);
    assert_eq!(fs::read(&fixture.selection).ok(), selected);
    assert!(!journal.exists());
}

fn recovery_failure_keeps_exact_target_for_next_retry() {
    let fixture = Releases::new("hamn-recovery-ownership-");
    let (script, initial) = fixture.release("1.0.1");
    fixture.run_update(&script, &initial, &["--bootstrap"]);
    let (original, selected) = (fixture.active(), fs::read(&fixture.selection).unwrap());
    let (_, pending) = fixture.release("1.0.2");
    let journal = interrupt(&fixture, &pending, "AFTER_GUEST_SELECTION");
    let attempted = fixture.active();
    let recorded = format!("{}\n", attempted.display());
    assert_eq!(fs::read_to_string(journal.join("new-target")).unwrap(), recorded);
    // The fault seam fails only the rollback link rename.
    let failed = recover(&fixture, None, false, &[("HAMN_TEST_UPDATE_FAULTS", "rollback-link")]);
    assert_ne!(failed.returncode, 0);
    assert!(failed.stderr().contains("could not be safely recovered"), "{}", failed.stderr());
    assert_eq!(fixture.active(), attempted);
    assert_eq!(fs::read_to_string(journal.join("new-target")).unwrap(), recorded);
    let recovered = recover(&fixture, None, false, &[]);
    assert_ne!(recovered.returncode, 0);
    assert!(recovered.stderr().contains("recovered the previous binary"), "{}", recovered.stderr());
    assert_eq!(fixture.active(), original);
    assert_eq!(fs::read(&fixture.selection).unwrap(), selected);
    assert!(!journal.exists());
}

fn generations(fixture: &Releases) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> =
        fs::read_dir(fixture.datadir.join(".hamn-generations")).unwrap().map(|entry| entry.unwrap().path()).collect();
    entries.sort();
    entries
}

/// The installer records the attempted target only into the journal its
/// own transaction published: there is no journal argument to hand it a
/// foreign one, and a journal replaced while the transaction waits is
/// refused before any generation is created or anything written into it.
fn installer_rejects_foreign_journal_without_writing_it() {
    let fixture = Releases::new("hamn-recovery-ownership-");
    let (script, initial) = fixture.release("1.0.1");
    fixture.run_update(&script, &initial, &["--bootstrap"]);
    let active = fixture.active();
    let foreign = fixture.root.join("foreign-journal");
    fs::create_dir(&foreign).unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(foreign.join("sentinel"), "must remain unchanged").unwrap();
    let before = snapshot(&foreign);
    let before_generations = generations(&fixture);
    let result = upgrade::run(
        Command::new(&script)
            .args(["__install-support", "install"])
            .arg(&script)
            .arg(&fixture.bindir)
            .arg(&fixture.datadir)
            .arg(&foreign)
            .env("HOME", &fixture.home),
        WAIT,
    );
    assert_eq!(result.returncode, 2, "{}", result.stderr());
    assert!(result.stderr().contains("usage: hamn __install-support install"), "{}", result.stderr());
    assert_eq!(snapshot(&foreign), before);
    assert_eq!(fixture.active(), active);
    assert_eq!(generations(&fixture), before_generations);

    // Replace the prepared journal with the foreign directory while the
    // update waits after publishing it.
    let (_, pending) = fixture.release("1.0.2");
    let barrier = Barrier::new(&fixture.root, "PREPARED", "prepared");
    let mut command = fixture.updater(&fixture.installed(), &pending, &[]);
    let child = upgrade::Group::spawn(barrier.apply(&mut command));
    barrier.await_ready(Duration::from_secs(20), "prepared transaction");
    let journal = fixture.journal();
    fs::rename(&journal, fixture.root.join("own-journal")).unwrap();
    fs::rename(&foreign, &journal).unwrap();
    barrier.release(WAIT);
    let result = child.finish(WAIT);
    assert_ne!(result.returncode, 0);
    assert!(result.stderr().contains("unsafe update journal handoff"), "{}", result.stderr());
    assert!(result.stderr().contains("host install failed and the recovery journal could not be applied"), "{}", result.stderr());
    assert_eq!(snapshot(&journal), before, "the installer wrote into a foreign journal");
    assert_eq!(fixture.active(), active);
    assert_eq!(generations(&fixture), before_generations, "a generation was created for a foreign journal");
}

/// Replaces the four legacy recovery cases: v1 journals (Hamn 0.1.2 and
/// earlier) and v2 journals (pre-release builds) record no attempted
/// generation, so they are refused, pending or retired, and left unchanged
/// together with the command link and the guest image selection.
fn journals_without_an_attempted_target_are_refused_unchanged() {
    let fixture = Releases::new("hamn-recovery-ownership-");
    let (script, initial) = fixture.release("1.0.1");
    fixture.run_update(&script, &initial, &["--bootstrap"]);
    let active = fixture.active();
    let cache = fixture.selection.parent().unwrap().to_path_buf();
    for (version, state) in [
        (1, "version=1\nbootstrap=0\nselection=present\n"),
        (2, "version=2\nbootstrap=0\nselection=present\nhostMutation=1\n"),
    ] {
        for (name, pending) in [(".hamn-update-transaction", true), (".hamn-update-recovered.Abc123", false)] {
            let journal = cache.join(name);
            fs::create_dir(&journal).unwrap();
            fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
            for (file, contents) in [
                ("state", state.as_bytes().to_vec()),
                ("attempt", b"Abc123\n".to_vec()),
                ("old-target", format!("{}\n", active.display()).into_bytes()),
                ("previous-selection", fs::read(&fixture.selection).unwrap()),
                ("new-selection", b"{}\n".to_vec()),
            ] {
                fs::write(journal.join(file), contents).unwrap();
                fs::set_permissions(journal.join(file), fs::Permissions::from_mode(0o600)).unwrap();
            }
            let before = snapshot(&cache);
            let result = recover(&fixture, None, false, &[]);
            assert_ne!(result.returncode, 0);
            let expected = if pending {
                format!(
                    "an interrupted update from Hamn 0.1.2 or earlier left a v{version} journal at {}, which this Hamn cannot recover",
                    journal.display()
                )
            } else {
                format!("a finished v{version} update journal from Hamn 0.1.2 or earlier remains at {}", journal.display())
            };
            assert!(result.stderr().contains(&expected), "{expected}: {}", result.stderr());
            assert_eq!(snapshot(&cache), before, "refusing a v{version} journal changed the cache");
            assert_eq!(fixture.active(), active);
            fs::remove_dir_all(&journal).unwrap();
        }
    }
}
