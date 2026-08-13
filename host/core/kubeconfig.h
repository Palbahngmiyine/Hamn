#ifndef HAMN_KUBECONFIG_H
#define HAMN_KUBECONFIG_H

#include "core/profile.h"
#include "core/state.h"

struct kubeconfig_context_snapshot {
    char name[128];
    int present;
};

/* Refuse a colliding non-Hamn context before changing guest K3s state. */
int kubeconfig_preflight_profile(const struct profile *profile);

/* Merge the profile-local K3s config into ~/.kube/config, select this
 * profile's owned context, and persist the previous current-context. */
int kubeconfig_activate_profile(const struct profile *profile,
                                struct vm_state *state);

/* Like kubeconfig_activate_profile(), while also retaining the current
 * context that existed before this activation.  The snapshot is only for an
 * in-flight host transaction; it must not be persisted. */
int kubeconfig_activate_profile_with_snapshot(
    const struct profile *profile, struct vm_state *state,
    struct kubeconfig_context_snapshot *snapshot);

/* Restore an activation snapshot only if this profile's context is still
 * active.  This avoids overwriting a context the user selected concurrently. */
int kubeconfig_restore_context_snapshot(
    const struct profile *profile,
    const struct kubeconfig_context_snapshot *snapshot);

/* Restore a context only when Hamn still owns the active profile context.
 * The Hamn-owned entries stay in ~/.kube/config so a later K3s start can
 * refresh them without rewriting foreign configuration. */
int kubeconfig_restore_previous(const struct profile *profile,
                                struct vm_state *state);

#endif
