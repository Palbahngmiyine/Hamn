//! Executes production cancellation-boundary functions with deterministic
//! remote fault injection: the deployment refresh and reconcile functions
//! are copied out of host/core/guest_deployment.c, compiled with C stubs
//! for their collaborators, and run.
//!
//! The Python suite's legacy K3s `retirement_run` case was not ported: the
//! product removes K3s retirement, its lock and its payload.
use crate::runner::{self, case};
use crate::support::c_extract;
use crate::support::tmp::TempDir;
use std::path::Path;
use std::process::{Command, ExitCode};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "remote-cancel-boundaries",
        "deployment refresh/reconcile cancellation boundaries recover or preserve cleanup",
        vec![case("deployment_refresh_and_reconcile", deployment_refresh_and_reconcile)],
        filters,
    )
}

fn deployment_refresh_and_reconcile() {
    let file = Path::new("host/core/guest_deployment.c");
    let functions: String =
        ["deployment_cancel_recover", "deployment_refresh_locked", "guest_deployment_reconcile_runtime_locked"]
            .iter()
            .map(|name| c_extract::function_in(file, name))
            .collect();
    compile_and_run("deployment", &format!("{DEPLOYMENT_PREFIX}{functions}{DEPLOYMENT_MAIN}"));
}

/// Writes `source` to `<name>.c` in a temporary directory, compiles it with
/// clang as C11 with implicit declarations as errors, and requires the
/// program to exit successfully (its assertions are the checks).
fn compile_and_run(name: &str, source: &str) {
    let temporary = TempDir::new_in(&std::env::temp_dir(), "hamn-cancel-boundaries-");
    let file = temporary.path().join(format!("{name}.c"));
    std::fs::write(&file, source).unwrap();
    let binary = file.with_extension("");
    let status = Command::new("clang")
        .args(["-std=c11", "-Werror=implicit-function-declaration"])
        .arg(&file)
        .arg("-o")
        .arg(&binary)
        .status()
        .expect("clang");
    assert!(status.success(), "clang {}: {status}", file.display());
    let status = Command::new(&binary).status().unwrap_or_else(|error| panic!("{}: {error}", binary.display()));
    assert!(status.success(), "{}: {status}", binary.display());
}

const DEPLOYMENT_PREFIX: &str = r#"
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
struct profile { int unused; };
struct vm_state { char ip[64]; };
#define GUEST_DEPLOYMENT_TOKEN_CAP 33
static int recovery_complete, cleanup_pending, cancel_recovered;
static int cancelled, depth, remote_active, barriers, fail_barrier, fail_recovery;
static int fixture_current, transactions, recovery_calls;
static const char *fault;
#define logerr(...) ((void)0)
#define logmsg(...) ((void)0)
#define guest_deployment_is_current(...) fixture_current
#define ssh_master_start(...) 0
#define guest_deployment_wait_cloud_init(...) 0
#define deployment_token_generate(...) ((void)0)
#define operation_phase(...) 0
#define remote_mutation_cleanup_pending() 0
#define guest_deployment_configure_runtime(...) 0
#define guest_deployment_configure_docker(...) 0
#define guest_deployment_forward_sockets(...) 0
#define guest_deployment_runtime_ready(...) 0
#define guest_deployment_mark_current(...) 0
static int proc_cancelled(void) { return cancelled && !depth; }
static void proc_cleanup_begin(void) { depth++; }
static void proc_cleanup_end(void) { assert(depth > 0); depth--; }
static int deployment_exec_locked(const struct profile *p, const char *ip, const char *const cmd[]) {
 (void)p; (void)ip; assert(depth && !strcmp(cmd[1], "true")); barriers++;
 if (fail_barrier) return -1;
 remote_active=0; return 0;
}
static int retirement_recover(const struct profile *p, const char *ip) {
 (void)p; (void)ip; recovery_calls++;
 assert(!remote_active); return fail_recovery && depth ? -1 : 0;
}
static int deployment_transaction(const struct profile *p, const char *ip, const char *action, const char *token) {
 (void)p; (void)ip; (void)token; transactions++;
 if (!strcmp(action, fault)) { cancelled=1; remote_active=1; return 130; }
 return 0;
}
"#;

const DEPLOYMENT_MAIN: &str = r#"
int main(void) {
 struct profile p={0}; struct vm_state state={"192.0.2.1"};
 const char *phases[]={"begin", "commit"};
 for (int reconcile=0; reconcile<2; reconcile++)
 for (int phase=0; phase<2; phase++)
 for (int failure=0; failure<3; failure++) {
  fault=phases[phase]; fixture_current=reconcile; cancelled=remote_active=depth=barriers=0;
  recovery_complete=cleanup_pending=recovery_calls=transactions=0;
  fail_barrier=failure==1; fail_recovery=failure==2;
  int rc=reconcile ? guest_deployment_reconcile_runtime_locked(&p,&state) : deployment_refresh_locked(&p,&state,1);
  assert(rc==-1 && cancelled && depth==0 && barriers==1);
  assert(cleanup_pending==fail_barrier && remote_active==fail_barrier);
  assert(recovery_calls==(!reconcile + !fail_barrier) && recovery_complete==(failure==0));
  assert(transactions==(phase ? 2 : 1));
 }
 puts("PASS: refresh/reconcile begin/commit cancellation, barrier failure and recovery failure");
}
"#;
