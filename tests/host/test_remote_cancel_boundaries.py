#!/usr/bin/env python3
"""Execute production boundary functions with deterministic remote fault injection."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def function(file, name):
    text = (ROOT / file).read_text()
    start = text.index(name + '(')
    start = text.rfind('\n', 0, start) + 1
    opening = text.index('{', start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end] + '\n'


prefix = r'''
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
'''
deployment = ''.join(function('host/core/guest_deployment.c', name) for name in (
    'deployment_cancel_recover', 'deployment_refresh_locked',
    'guest_deployment_reconcile_runtime_locked'))
main = r'''
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
'''
retirement_prefix = r'''
#include <assert.h>
#include <stdio.h>
struct profile { int unused; };
static int cancel_recovered, cleanup_pending, depth, calls, failed, cancelled;
#define logerr(...) ((void)0)
#define operation_phase(...) 0
static int proc_cancelled(void) { return cancelled && !depth; }
static void proc_cleanup_begin(void) { depth++; }
static void proc_cleanup_end(void) { depth--; }
static int retirement_execute(struct profile *profile, const char *ip) {
 (void)profile; (void)ip; calls++;
 if (calls==1) { cancelled=1; return -1; }
 assert(depth && !proc_cancelled()); return failed ? -1 : 0;
}
'''
retirement_main = r'''
int main(void) {
 struct profile p={0};
 for (failed=0; failed<2; failed++) {
  calls=cancelled=depth=0;
  assert(retirement_run(&p,"192.0.2.1")==-1);
  assert(calls==2 && depth==0 && cleanup_pending==failed && cancel_recovered==!failed);
 }
 puts("PASS: retirement cancellation waits for journal resume and preserves unresolved cleanup");
}
'''
with tempfile.TemporaryDirectory(prefix='hamn-cancel-boundaries-') as temporary:
    for name, source in (
        ('deployment', prefix + deployment + main),
        ('retirement', retirement_prefix + function('host/core/retirement.c', 'retirement_run') + retirement_main),
    ):
        file = Path(temporary) / (name + '.c')
        file.write_text(source)
        binary = file.with_suffix('')
        subprocess.run(['clang', '-std=c11', '-Werror=implicit-function-declaration', str(file), '-o', str(binary)], check=True)
        subprocess.run([str(binary)], check=True)
