#ifndef HAMN_CONTROL_LOCKED_H
#define HAMN_CONTROL_LOCKED_H

/* Profile operations of core/control.h for a caller that already holds the
 * profile's lifecycle lock (core/lifecycle.h), so that one control call can
 * run several of them while no other operation can change the profile in
 * between. Each is the control call of the same name without taking or
 * releasing that lock, and without the argument checks that come before
 * the lock: the caller passes a valid profile name, a memory_gib of at most
 * UINT_MAX / 1024, and non-NULL pointers. The return values, results, log
 * and effects are those of the control call. The lifecycle lock is not
 * reentrant: none of these may be called by a control call that takes it. */

/* hamn_control_start. *refusal is written on every return. */
int control_start_locked(const char *profile, unsigned cpus,
                         unsigned memory_gib, unsigned disk_gib,
                         char **refusal);
/* hamn_control_stop. */
int control_stop_locked(const char *profile);
/* hamn_control_purge, apart from its removal of what interrupted purges
 * left (purge_sweep). *absent is written on every return. */
int control_purge_locked(const char *profile, int *absent);

#endif
