#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#include "core/control.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"
#include "core/purge.h"
#include "util/fs.h"

/* Soft deletion: stop the VM and mark the profile deleted. Its disk and
 * Docker data are preserved; only system uninstall removes them. */
static int delete_locked(const char *profile_name)
{
    struct profile profile;
    char failure[PROFILE_FAILURE_CAP];
    if (profile_load_explained(&profile, profile_name, failure) != 0) {
        /* Before the VM is touched and the marker written: a refusal with a
         * result, not the end of the process. */
        logerr("%s", failure);
        return 1;
    }
    int mutation = profile_mutation_lock(&profile);
    if (mutation < 0) {
        logerr("another %s profile mutation is running", profile.name);
        return 1;
    }
    int stop_result = vm_stop(&profile, NULL);
    if (stop_result != VM_STOP_OK) {
        profile_mutation_unlock(mutation);
        die("refusing to delete while VM or host port-forward ownership is "
            "uncertain");
    }
    char deleted[PROFILE_PATH_CAP];
    if (!profile_path(&profile, "deleted", deleted, sizeof(deleted)) ||
        fs_write_file_atomic(deleted, "soft-deleted\n", 13, 0600) != 0) {
        logerr("cannot mark the soft-deleted profile: %s", strerror(errno));
        profile_mutation_unlock(mutation);
        return 1;
    }
    profile_mutation_unlock(mutation);
    logmsg("deleted VM for profile %s; its disk data is preserved", profile.name);
    return 0;
}

int hamn_control_delete(const char *profile_name)
{
    if (!profile_name_valid(profile_name))
        return 2;
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0)
        return 1;
    int rc = delete_locked(profile_name);
    vm_lifecycle_lock_release(&lock);
    return rc;
}

/* Whether ~/.hamn/<profile_name> exists, found with lstat alone: a profile
 * that is not there gets no directory, lock file or ~/.hamn. Returns 1, 0,
 * or -1 having logged why. */
static int profile_present(const char *profile_name)
{
    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.name, sizeof(profile.name), "%s", profile_name);
    struct stat status;
    if (profile_locate(&profile) == 0 && lstat(profile.dir, &status) == 0)
        return 1;
    if (errno == ENOENT)
        return 0;
    logerr("cannot look for profile %s: %s", profile_name, strerror(errno));
    return -1;
}

static int purge_locked(const char *profile_name, int *absent)
{
    /* Another purge can have finished while this one waited for the lock. */
    int present = profile_present(profile_name);
    if (present <= 0) {
        *absent = present == 0;
        return present == 0 ? 0 : 1;
    }
    struct profile profile;
    char failure[PROFILE_FAILURE_CAP];
    if (profile_load_explained(&profile, profile_name, failure) != 0) {
        logerr("%s", failure);
        return 1;
    }
    int mutation = profile_mutation_lock(&profile);
    if (mutation < 0) {
        logerr("another %s profile mutation is running", profile.name);
        return 1;
    }
    /* As vm reset before it deletes a disk: the stop succeeded and the VM
     * process is known to be gone. */
    if (vm_stop(&profile, NULL) != VM_STOP_OK ||
        vm_process_probe(&profile, NULL) != VM_PROCESS_STALE) {
        profile_mutation_unlock(mutation);
        logerr("refusing to delete while VM or host port-forward ownership is "
               "uncertain");
        return HAMN_CONTROL_STOP_UNCERTAIN;
    }
    enum purge_result result = purge_retire(&profile);
    profile_mutation_unlock(mutation);
    if (result != PURGE_REMOVED)
        return 1;
    logmsg("deleted profile %s with its disk and Docker data", profile.name);
    return 0;
}

int hamn_control_purge(const char *profile_name, int *absent)
{
    if (!absent)
        return 2;
    *absent = 0;
    if (!profile_name_valid(profile_name))
        return 2;
    purge_sweep();
    int present = profile_present(profile_name);
    int rc = present < 0 ? 1 : 0;
    if (present == 0) {
        *absent = 1;
    } else if (present > 0) {
        struct vm_lifecycle_lock lock;
        if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0) {
            logerr("cannot lock the %s profile lifecycle", profile_name);
            return 1;
        }
        rc = purge_locked(profile_name, absent);
        vm_lifecycle_lock_release(&lock);
    }
    if (rc == 0 && *absent)
        logmsg("profile %s does not exist; nothing to delete", profile_name);
    return rc;
}
