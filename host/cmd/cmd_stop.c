#include <stdio.h>

#include "core/control.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/operation.h"
#include "core/mutation_lock.h"
#include "core/profile.h"

static int cmd_stop_locked(const char *profile_name)
{
    struct profile profile;
    if (profile_read_existing(&profile, profile_name) != 0) {
        logerr("cannot load profile");
        return 1;
    }
    int mutation_fd = profile_mutation_lock(&profile);
    if (mutation_fd < 0) {
        logerr("another %s profile mutation is running", profile.name);
        return 1;
    }

    if (operation_begin(&profile, "vm stop") != 0) {
        profile_mutation_unlock(mutation_fd);
        return 1;
    }
    int was_running = 0;
    if (vm_stop(&profile, &was_running) != 0) {
        profile_mutation_unlock(mutation_fd);
        return operation_finish(1, 0);
    }
    profile_mutation_unlock(mutation_fd);
    logmsg(was_running ? "stopped" : "not running");
    return operation_finish(0, 1);
}

int hamn_control_stop(const char *profile_name)
{
    if (!profile_name_valid(profile_name))
        return 2;
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0) {
        logerr("cannot lock the %s profile lifecycle", profile_name);
        return 1;
    }
    int rc = cmd_stop_locked(profile_name);
    vm_lifecycle_lock_release(&lock);
    return rc;
}
