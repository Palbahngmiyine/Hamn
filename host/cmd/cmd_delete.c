#include <errno.h>
#include <stdio.h>
#include <string.h>

#include "core/control.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"
#include "util/fs.h"

/* Soft deletion: stop the VM and mark the profile deleted. Its disk and
 * Docker data are preserved; only system uninstall removes them. */
static int delete_locked(const char *profile_name)
{
    struct profile profile;
    if (profile_load(&profile, profile_name) != 0)
        die("cannot load profile");
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
