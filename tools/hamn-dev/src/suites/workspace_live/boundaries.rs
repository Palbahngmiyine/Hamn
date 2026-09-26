//! Cancellation barriers inside the owned VM. A one-shot `flock` wrapper,
//! installed in /usr/local/bin ahead of /usr/bin/flock for one case, gates
//! the product's own deployment transaction at `begin` or `commit`, either
//! while it holds the deployment lock or queued before the lock. No signed
//! helper is replaced, and only the exact wrapper installed is removed.
//!
//! The guest runs the barrier as bash scripts in a root-only directory:
//! - `gate.sh COMMAND...` ignores HUP, TERM and INT (so the gated dispatch
//!   outlives the frontend cancelled around it), marks `ready`, waits for
//!   `released`, runs COMMAND and records its status in `done`.
//! - `wait.sh MARKER` waits for a marker file. Hamn keeps no Python, and the
//!   guest image has no inotify tools, so the marker is polled; markers are
//!   files, so one written before the wait starts is never missed.
use super::cancellation::{headless_start, wait_record};
use super::{Live, Must, PROFILE, communicate, finally, read_record, running, signal_child, snapshot};
use crate::release::process::{self, Spec};
use crate::release::syntax::{shell_join, shell_quote};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::process::Child;
use std::time::Duration;

pub(crate) const WRAPPER: &str = "/usr/local/bin/flock";
pub(crate) const HELPERS: &str = "/usr/local/libexec/hamn";
pub(crate) const LOCK: &str = "/run/hamn-deployment.lock";

/// The programs the generated scripts hand over to: the guest's, or
/// recorders in `workspace-live-checks`.
pub(crate) struct Programs<'a> {
    pub flock: &'a str,
    pub shell: &'a str,
}

pub(crate) const GUEST: Programs<'static> = Programs { flock: "/usr/bin/flock", shell: "/bin/bash" };

/// The `flock` wrapper: the first invocation that runs the deployment
/// transaction's `action` under the deployment lock claims `directory` and
/// runs through `gate.sh`, inside the lock or (`before_lock`) before it.
/// Every other invocation reaches the real flock unchanged.
pub(crate) fn wrapper_source(directory: &str, action: &str, before_lock: bool, programs: &Programs) -> String {
    let (root, flock, shell) = (shell_quote(directory), shell_quote(programs.flock), shell_quote(programs.shell));
    format!(
        r#"#!/bin/bash
# A one-shot Hamn test barrier around the deployment lock.
root={root}
action={action}
before_lock={before_lock}
lock={LOCK}
args=("$@")
count=$#
index=-1
for ((i = 0; i < count; i++)); do
    if [ "${{args[i]}}" = "$lock" ]; then
        index=$i
        break
    fi
done
if [ "$index" -ge 0 ] && [ "$count" -gt 2 ] && [ "${{args[count-2]}}" = "$action" ] &&
    [[ "${{args[count-3]}}" == */guest-deployment-transaction ]]; then
    if mkdir "$root/claimed" 2>/dev/null; then
        if [ "$before_lock" = 1 ]; then
            exec {shell} "$root/gate.sh" {flock} "$@"
        fi
        exec {flock} "${{args[@]:0:index+1}}" {shell} "$root/gate.sh" "${{args[@]:index+1}}"
    elif [ ! -d "$root/claimed" ]; then
        echo "flock barrier: cannot claim $root" >&2
        exit 1
    fi
fi
exec {flock} "$@"
"#,
        action = shell_quote(action),
        before_lock = u8::from(before_lock),
    )
}

/// `gate.sh COMMAND...` (see the module documentation).
pub(crate) fn gate_source(directory: &str, programs: &Programs) -> String {
    let (root, shell) = (shell_quote(directory), shell_quote(programs.shell));
    format!(
        r#"#!/bin/bash
root={root}
trap '' HUP TERM INT
printf 'LOCK_READY\n' > "$root/ready.tmp" && mv "$root/ready.tmp" "$root/ready" || exit 1
{shell} "$root/wait.sh" released > /dev/null 2>&1 || exit 1
"$@"
code=$?
printf '%s' "$code" > "$root/done.tmp" && mv "$root/done.tmp" "$root/done" || exit 1
exit "$code"
"#
    )
}

/// `wait.sh MARKER`: waits up to `deadline_seconds` for the marker, then
/// prints LOCK_READY (for `ready`) or RELEASED.
pub(crate) fn wait_source(directory: &str, deadline_seconds: u32) -> String {
    let root = shell_quote(directory);
    format!(
        r#"#!/bin/bash
root={root}
deadline=$((SECONDS + {deadline_seconds}))
until [ -e "$root/$1" ]; do
    if [ "$SECONDS" -ge "$deadline" ]; then
        echo "guest gate deadline exceeded: $1" >&2
        exit 1
    fi
    sleep 0.05
done
if [ "$1" = ready ]; then echo LOCK_READY; else echo RELEASED; fi
"#
    )
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// An installed barrier for one case.
pub(crate) struct BoundaryGate<'a> {
    live: &'a Live,
    pub directory: String,
    source: String,
    released: bool,
}

impl<'a> BoundaryGate<'a> {
    pub(crate) fn new(live: &'a Live, action: &str, before_lock: bool) -> Self {
        let directory = format!("/var/lib/hamn-workspace-cancel-{}", super::random_hex(16));
        let source = wrapper_source(&directory, action, before_lock, &GUEST);
        let quoted = shell_quote(&directory);
        live.ssh(&format!(
            "test ! -e {WRAPPER}; test ! -L {WRAPPER}
test \"$(command -v flock)\" = /usr/bin/flock
mkdir -m 700 {quoted}
printf %s {wait} > {quoted}/wait.sh
printf %s {gate} > {quoted}/gate.sh
printf %s {wrapper} > {WRAPPER}
chmod 755 {WRAPPER}
test \"$(command -v flock)\" = {WRAPPER}
",
            wait = shell_quote(&wait_source(&directory, 180)),
            gate = shell_quote(&gate_source(&directory, &GUEST)),
            wrapper = shell_quote(&source),
        ));
        Self { live, directory, source, released: false }
    }

    /// Waits up to 180 seconds for the gated dispatch to mark `ready`.
    pub(crate) fn wait(&mut self) {
        let status = self.live.call(&["vm", "status"], &[]);
        let ip = status["ip"].as_str().expect("VM status has no IP address");
        let key = self.live.profile().join("id_ed25519");
        let wait = format!("{}/wait.sh", self.directory);
        let remote = shell_join(&["sudo", GUEST.shell, &wait, "ready"]);
        let mut args: Vec<&OsStr> = vec!["-F".as_ref(), "none".as_ref(), "-i".as_ref(), key.as_os_str()];
        for option in [
            "BatchMode=yes",
            "IdentitiesOnly=yes",
            "ConnectTimeout=10",
            "StrictHostKeyChecking=no",
            "UserKnownHostsFile=/dev/null",
        ] {
            args.extend(["-o".as_ref(), OsStr::new(option)]);
        }
        let host = format!("hamn@{ip}");
        args.extend([OsStr::new(&host), OsStr::new(&remote)]);
        let spec = Spec { environment: Some(&self.live.runtime.environment), input: None };
        let output = process::capture(OsStr::new("/usr/bin/ssh"), &args, &spec, Duration::from_secs(180)).must();
        assert!(
            output.status.success() && output.stdout == b"LOCK_READY\n",
            "the gate did not become ready: {:?} {}",
            String::from_utf8_lossy(&output.stdout),
            output.stderr_lossy()
        );
    }

    pub(crate) fn release(&mut self) {
        if !self.released {
            self.live.ssh(&format!("touch {}/released", shell_quote(&self.directory)));
            self.released = true;
        }
    }

    /// Releases the gate and removes exactly the wrapper installed, after
    /// the deployment lock is free; a changed wrapper is never removed.
    pub(crate) fn close(&mut self) {
        self.release();
        let expected = sha256_hex(self.source.as_bytes());
        self.live.ssh(&format!(
            "test \"$(sha256sum {WRAPPER} | cut -d' ' -f1)\" = {expected}
/usr/bin/flock --wait 120 {LOCK} true
rm {WRAPPER}
rm -r {}
",
            shell_quote(&self.directory)
        ));
    }

    /// Checks that the gated dispatch ran after release and was rejected
    /// with 130.
    pub(crate) fn assert_late_dispatch_rejected(&self) {
        let directory = shell_quote(&self.directory);
        self.live.ssh(&format!(
            "{shell} {directory}/wait.sh done; test \"$(cat {directory}/done)\" = 130",
            shell = GUEST.shell
        ));
    }
}

pub(crate) fn helper_hashes_command() -> String {
    format!(
        "sha256sum {HELPERS}/verify-image-contract {HELPERS}/guest-deployment-transaction {HELPERS}/configure-docker"
    )
}

pub(crate) const NO_BACKUPS: &str = "test -z \"$(ls -A /var/lib/hamn/deployment-transactions)\"";

/// Refresh and reconcile, cancelled at `begin` and `commit` while holding
/// the lock and (refresh only) queued before it: the frontend waits for the
/// remote writer, a late dispatch is rejected, and Docker data, the VM and
/// the signed helpers are preserved.
pub(crate) fn cancellation_boundaries(live: &Live) {
    live.assert_owned();
    let path = live.profile().join("operation.json");
    let before = snapshot(live);
    let hashes = live.ssh(&helper_hashes_command());
    let mut evidence: Vec<Value> = Vec::new();
    let cases = [("refresh", "begin"), ("refresh", "commit"), ("reconcile", "begin"), ("reconcile", "commit")];
    let held = cases.iter().map(|&(mode, action)| (mode, action, false));
    let queued = cases.iter().filter(|(mode, _)| *mode != "reconcile").map(|&(mode, action)| (mode, action, true));
    for (mode, action, queued) in held.chain(queued) {
        live.call(&["vm", "start"], &["--yes"]);
        let original_pid = live.vm_pid();
        let mut case = (BoundaryGate::new(live, action, queued), None::<Child>);
        finally(
            &mut case,
            |(gate, child)| {
                if mode == "refresh" {
                    let version = live.profile().join("guest-deployment.version");
                    match std::fs::remove_file(&version) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => panic!("{}: {error}", version.display()),
                    }
                } else {
                    live.call(&["vm", "stop"], &["--yes"]);
                }
                let previous = read_record(&path)["operationId"].clone();
                let child = child.insert(headless_start(live, PROFILE, &[]));
                wait_record(
                    &path,
                    |value| {
                        value["operationId"] != previous
                            && (mode != "reconcile" || value["phase"] == "guest-deployment-reused")
                    },
                    Duration::from_secs(180),
                    None,
                );
                gate.wait();
                signal_child(child, libc::SIGINT);
                let (status, stdout, stderr) = if queued {
                    // The original dispatch has not acquired the lock. Cleanup
                    // may finish first, but its retained token must reject
                    // that dispatch.
                    let output = communicate(child, Duration::from_secs(240));
                    gate.release();
                    output
                } else {
                    wait_record(&path, |value| value["phase"] == "fencing-after-cancel", Duration::from_secs(30), None);
                    live.ssh(&format!("! /usr/bin/flock -n {LOCK} true"));
                    assert!(running(child), "frontend exited while original writer held its lock");
                    gate.release();
                    communicate(child, Duration::from_secs(240))
                };
                let record = read_record(&path);
                assert!(!status.success() && record["status"] == "cancelled", "{record} {stdout} {stderr}");
                if mode != "reconcile" {
                    gate.assert_late_dispatch_rejected();
                }
                if mode == "reconcile" {
                    assert_eq!(live.state(), "stopped");
                    live.call(&["vm", "start"], &["--yes"]);
                } else {
                    assert_eq!(live.state(), "running");
                    assert_eq!(live.vm_pid(), original_pid, "cancellation replaced the VM");
                }
                live.ssh(NO_BACKUPS);
                assert_eq!(snapshot(live), before, "cancellation changed Docker data");
                assert_eq!(live.ssh(&helper_hashes_command()), hashes, "a signed helper changed");
                evidence.push(json!({"mode": mode, "action": action, "queuedBeforeLock": queued, "operation": record}));
                println!(
                    "PASS: actual {mode}/{action} queued={queued} cancellation waits for remote writer and preserves Docker data"
                );
            },
            |(gate, child)| {
                // Reach a running owned VM before SSH cleanup if cancellation
                // stopped it.
                if live.state() == "stopped" {
                    live.call(&["vm", "start"], &["--yes"]);
                }
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
    live.write_json("cancel-boundary-results.json", &Value::from(evidence));
}
