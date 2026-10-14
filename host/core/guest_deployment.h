#ifndef HAMN_GUEST_DEPLOYMENT_H
#define HAMN_GUEST_DEPLOYMENT_H

#include "core/profile.h"
#include "core/state.h"

/*
 * The marker records which host release last installed the guest helpers.
 * Returns 1 when current, 0 when absent/stale, and -1 for an unsafe or
 * unreadable marker.
 */
int guest_deployment_is_current(const struct profile *profile);
/* Roll back a ready backup left by an interrupted guest transaction, or
 * discard one that is not ready (see core/deployment_recovery.h), under the
 * guest deployment lock; a discard is logged. Returns 0 when no backup
 * remains. After a cancelled attempt, recovery_complete reports whether a
 * second recovery finished and Docker became ready; the caller must hold the
 * profile mutation lock. */
int guest_deployment_recover(const struct profile *profile, const char *ip);
int guest_deployment_recovery_complete(void);
/* A remote writer may still be active; do not stop the VM yet. */
int guest_deployment_cleanup_pending(void);
int guest_deployment_mark_current(const struct profile *profile);

int guest_deployment_configure_runtime(const struct profile *profile,
                                       const char *ip);
int guest_deployment_forward_sockets(const struct profile *profile,
                                     const char *ip);
int guest_deployment_docker_ready(const struct profile *profile);
/* Readiness budget for a VM that is already running. Every probe runs within
 * the budget, so it must hold one complete probe round of a healthy guest
 * that answers slowly: a VM judged not ready here is stopped as an
 * incomplete start. */
#define GUEST_DEPLOYMENT_RUNNING_READY_TIMEOUT_SEC 5
/* Polls until the forwarded Docker API answers _ping and the guest agent,
 * Docker and system containerd probes succeed. timeout_sec (at least 1) is a
 * deadline for the whole wait: every probe runs with what is left of it, and
 * its client is reaped before this returns; a guest command that outlives its
 * client is not stopped, but the probes only read state. Returns 0 when
 * ready; -1 when the budget ends, on cancellation outside cleanup (errno
 * ECANCELED), or for a budget below 1 (EINVAL). */
int guest_deployment_runtime_ready(const struct profile *profile,
                                   const char *ip, int timeout_sec);

/* Caller must hold the profile mutation lock. */
int guest_deployment_refresh_locked(const struct profile *profile,
                                    const struct vm_state *state);
/* Reapply network-dependent Docker settings without modifying the guest image. */
int guest_deployment_reconcile_runtime_locked(
    const struct profile *profile, const struct vm_state *state);
int guest_deployment_repair_locked(const struct profile *profile,
                                   const struct vm_state *state);

#endif
