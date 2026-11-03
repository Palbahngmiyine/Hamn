#ifndef HAMN_PURGE_H
#define HAMN_PURGE_H

#include "core/profile.h"

/* How a purged profile directory leaves ~/.hamn. It is first renamed to
 * ~/.hamn/.purge-<name>-<16 lowercase hex digits>, which is no profile name,
 * so every listing and profile operation stops seeing the profile at once;
 * the rename is made durable before anything inside is removed, so a crash
 * cannot bring a partly removed tree back under the profile's name. Then the
 * renamed tree is removed. Entries are removed, never followed: a symbolic
 * link inside the tree goes, its target stays. Removal does not cross into
 * another mounted volume. */

enum purge_result {
    /* The profile directory and everything in it are gone. */
    PURGE_REMOVED = 0,
    /* Nothing changed: the profile directory is still at its name. */
    PURGE_UNCHANGED,
    /* The directory left its name, but the renamed tree, or part of it,
     * is still there. purge_sweep removes it. */
    PURGE_HIDDEN,
};

/* Move profile->dir out of the profile namespace and remove it, as above.
 * Call only while holding the profile's lifecycle and mutation locks, with
 * its VM known to be stopped. A failure is logged with logerr. */
enum purge_result purge_retire(const struct profile *profile);

/* Remove every directory of ~/.hamn whose name has the shape above and that
 * is this user's, left by a purge that was interrupted or failed. A tree is
 * skipped while a process holds the lifecycle lock of its profile name: the
 * purge that renamed it holds that lock until it has removed it. Takes no
 * lock otherwise, so two sweeps can remove one tree together. Does nothing
 * when ~/.hamn is missing or not a directory that only this user can write.
 * Never fails its caller and never records an error: a tree it cannot
 * remove stays, and it says so with logmsg. */
void purge_sweep(void);

#ifdef HAMN_TEST
/* One-shot fault: purge_retire stops right after its rename is durable, as
 * an interrupted purge does. Never present in production builds. */
void purge_test_interrupt_after_rename(void);
#endif
#endif
