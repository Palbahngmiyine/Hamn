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
int guest_deployment_mark_current(const struct profile *profile);

int guest_deployment_configure_runtime(const struct profile *profile,
                                       const char *ip);
int guest_deployment_forward_sockets(const struct profile *profile,
                                     const char *ip);
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
