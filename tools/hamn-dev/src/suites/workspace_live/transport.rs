//! A transport failure in the owned VM: the SSH child running a start's
//! deployment `begin` is killed while the barrier holds the dispatch queued
//! before the lock. The start fails (not cancelled), fences the late
//! dispatch, keeps Docker data and recovers the engine.
//!
//! Only the one verified SSH child is signalled, never a process group: it
//! must descend (at most eight levels) from the start's recorded worker, run
//! /usr/bin/ssh with the profile's key and control socket, and carry the
//! transaction's `begin` under the deployment lock; every identity and its
//! arguments are read again before the signal.
use super::boundaries::{BoundaryGate, HELPERS, LOCK, NO_BACKUPS, helper_hashes_command};
use super::cancellation::{headless_start, wait_record};
use super::processes::{Identity, System, Table};
use super::{
    Live, Must, PROFILE, communicate, finally, read_record, resolved, run_env, running, signal_child, snapshot,
};
use crate::support::pty;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Child;
use std::time::Duration;

/// The operation's worker: a direct child of `frontend` running `binary`
/// whose birth and executable UUID are those `record` names.
pub(crate) fn owned_worker(
    table: &impl Table,
    frontend: u32,
    record: &Value,
    binary: &Path,
) -> Result<Identity, String> {
    let pid = record["pid"].as_i64().and_then(|pid| i32::try_from(pid).ok()).ok_or("the record names no worker")?;
    let worker = table.identity(pid)?;
    if i64::from(worker.ppid) != i64::from(frontend) || resolved(Path::new(&worker.path)) != resolved(binary) {
        return Err(format!("process {pid} is not the frontend's worker running the candidate"));
    }
    let recorded = (record["startSec"].as_u64(), record["startUsec"].as_u64(), record["executableUuid"].as_str());
    if recorded != (Some(worker.start_sec), Some(worker.start_usec), Some(worker.executable_uuid.as_str())) {
        return Err(format!("process {pid} is not the recorded worker"));
    }
    Ok(worker)
}

/// The single SSH child that runs the recorded start's deployment `begin`
/// (see the module documentation).
pub(crate) fn owned_ssh(
    table: &impl Table,
    frontend: u32,
    record: &Value,
    binary: &Path,
    profile: &Path,
) -> Result<Identity, String> {
    let worker = owned_worker(table, frontend, record, binary)?;
    let key = profile.join("id_ed25519").to_string_lossy().into_owned();
    let control = format!("ControlPath={}", profile.join("ssh.sock").display());
    let transaction = format!("{HELPERS}/guest-deployment-transaction");
    let mut candidates: Vec<(Vec<Identity>, Vec<String>)> = Vec::new();
    let mut pending = vec![(worker.clone(), vec![worker])];
    while let Some((parent, chain)) = pending.pop() {
        if chain.len() > 8 {
            return Err("unexpected process ancestry".into());
        }
        for pid in table.children(parent.pid)? {
            let child = table.identity(pid)?;
            if child.ppid != parent.pid {
                return Err(format!("process {pid} is not a child of {}", parent.pid));
            }
            let mut descent = chain.clone();
            descent.push(child.clone());
            if child.path != "/usr/bin/ssh" {
                pending.push((child, descent));
                continue;
            }
            let args = table.argv(pid)?;
            let Some(remote) = args.last() else { continue };
            if args.contains(&key) && args.contains(&control) && remote.contains(&transaction) {
                let words = shlex_split(remote)?;
                if words.len() >= 3 && words[words.len() - 2] == "begin" && words.iter().any(|word| word == LOCK) {
                    candidates.push((descent, args));
                }
            }
        }
    }
    let [(chain, args)] = <[_; 1]>::try_from(candidates)
        .map_err(|_| "must identify exactly one owned deployment SSH child".to_owned())?;
    for identity in &chain {
        if table.identity(identity.pid)? != *identity {
            return Err("process identity changed".into());
        }
    }
    let ssh = chain.last().expect("a nonempty chain");
    if table.argv(ssh.pid)? != args {
        return Err("SSH arguments changed".into());
    }
    Ok(ssh.clone())
}

/// Python's `shlex.split` (POSIX mode, no comments): words split at
/// whitespace, single quotes literal, and in double quotes a backslash
/// escaping only `"` and `\`.
pub(crate) fn shlex_split(text: &str) -> Result<Vec<String>, String> {
    let (mut words, mut word, mut in_word) = (Vec::new(), String::new(), false);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next().ok_or("No closing quotation")? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next().ok_or("No closing quotation")? {
                        '"' => break,
                        '\\' => match chars.next().ok_or("No closing quotation")? {
                            c @ ('"' | '\\') => word.push(c),
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next().ok_or("No escaped character")?);
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// Kills the owned deployment SSH child of a queued `begin` (see the module
/// documentation).
pub(crate) fn transport_failure(live: &Live) {
    live.assert_owned();
    let profile = live.profile();
    let path = profile.join("operation.json");
    live.call(&["vm", "start"], &["--yes"]);
    let (original_pid, before) = (live.vm_pid(), snapshot(live));
    let hashes = live.ssh(&helper_hashes_command());
    live.ssh(NO_BACKUPS);
    let previous = read_record(&path).get("operationId").cloned().expect("the last operation's identifier");
    let mut case = (BoundaryGate::new(live, "begin", true), None::<Child>);
    finally(
        &mut case,
        |(gate, child)| {
            let version = profile.join("guest-deployment.version");
            match std::fs::remove_file(&version) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{}: {error}", version.display()),
            }
            let child = child.insert(headless_start(live, PROFILE, &[]));
            let active = wait_record(&path, |value| value["operationId"] != previous, Duration::from_secs(180), None);
            gate.wait();
            let current = read_record(&path);
            assert!(current["operationId"] == active["operationId"] && current["status"] == "running", "{current}");
            assert!(running(child), "the start ended before its transport failed");
            let target = owned_ssh(&System, child.id(), &active, &live.runtime.binary, &profile).must();
            // Only the verified SSH child, never its process group.
            pty::kill(target.pid as u32, libc::SIGKILL);
            // The original dispatch remains gated.
            let (status, stdout, stderr) = communicate(child, Duration::from_secs(240));
            let record = read_record(&path);
            assert!(!status.success() && record["status"] != "cancelled", "{record} {stdout} {stderr}");
            assert!(stderr.contains("fencing-after-failure") || record["phase"] == "fencing-after-failure", "{record}");
            gate.release();
            gate.assert_late_dispatch_rejected();
            live.ssh(NO_BACKUPS);
            assert_eq!(live.vm_pid(), original_pid, "the transport failure replaced the VM");
            assert_eq!(snapshot(live), before, "the transport failure changed Docker data");
            assert_eq!(live.ssh(&helper_hashes_command()), hashes, "a signed helper changed");
            let ready = live.call(&["vm", "start"], &["--yes"]);
            assert_eq!(ready["dockerStatus"], "ready", "{ready}");
            let socket = profile.join("docker.sock");
            let ping = run_env(
                "/usr/bin/curl",
                &[
                    "--silent",
                    "--show-error",
                    "--fail",
                    "--unix-socket",
                    super::path_str(&socket),
                    "http://localhost/_ping",
                ],
                &live.runtime.environment,
                Duration::from_secs(60),
                None,
            )
            .must();
            assert!(ping == "OK" && live.vm_pid() == original_pid, "engine did not recover: {ping:?}");
            let after = snapshot(live);
            assert_eq!(after, before, "recovery changed Docker data");
            let helpers: String = Sha256::digest(hashes.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
            live.write_json(
                "transport-failure-results.json",
                &json!({"ssh": target, "operation": record, "lateDispatchExit": 130, "dockerPing": "OK",
                    "before": before, "after": after, "helpersSha256": helpers}),
            );
            println!("PASS: owned SSH transport failure fences late begin, preserves Docker data and recovers /_ping");
        },
        |(gate, child)| {
            gate.release();
            if let Some(child) = child
                && running(child)
            {
                signal_child(child, libc::SIGINT);
                communicate(child, Duration::from_secs(240));
            }
            gate.close();
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shlex_split_follows_posix_quoting() {
        assert_eq!(
            shlex_split(r#"sudo flock '/run/a b' "x\"y\z" c\ d ''"#).unwrap(),
            ["sudo", "flock", "/run/a b", "x\"y\\z", "c d", ""]
        );
        assert_eq!(shlex_split("  a\t b\n").unwrap(), ["a", "b"]);
        for invalid in ["'open", "\"open", "trailing\\"] {
            assert!(shlex_split(invalid).is_err(), "{invalid}");
        }
    }
}
