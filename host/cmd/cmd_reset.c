#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "core/control.h"
#include "core/guest_deployment.h"
#include "core/guest_filesystem.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/operation.h"
#include "core/profile.h"
#include "util/fs.h"

static int directory_sync(const struct profile *profile)
{
    int fd = open(profile->dir, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    int rc = fsync(fd);
    int saved = errno;
    close(fd);
    errno = saved;
    return rc;
}

static int remove_profile_file(const struct profile *profile, const char *name,
                               int *existed)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, name, path, sizeof(path)))
        return -1;
    if (existed)
        *existed = access(path, F_OK) == 0;
    return fs_unlink_if_exists(path);
}

/* Deletes what belongs to the disk, each removal durable before the next,
 * so that an interruption leaves a state a start handles and a retry
 * finishes. The deployment marker goes first: a start that still finds the
 * old disk deploys again, and checks the disk before that. The disk goes
 * after its EFI variables, so a disk that is still there means the reset
 * has not finished. The record of its damage goes last, and until then it
 * names a disk that no longer exists. Returns 0, or -1 having logged what
 * is left. */
static int remove_disk(const struct profile *profile, int *removed_disk)
{
    const char *failed = NULL;
    if (guest_deployment_forget(profile) != 0 || directory_sync(profile) != 0)
        failed = "the guest deployment marker";
    else if (remove_profile_file(profile, "efi-vars.bin", NULL) != 0 ||
             directory_sync(profile) != 0)
        failed = "efi-vars.bin";
    else if (remove_profile_file(profile, "disk.img", removed_disk) != 0 ||
             directory_sync(profile) != 0)
        failed = "disk.img";
    else if (guest_filesystem_forget(profile) != 0 ||
             directory_sync(profile) != 0)
        failed = "the record of the disk's damage";
    if (!failed)
        return 0;
    logerr("cannot remove %s of profile %s: %s; run vm reset again", failed,
           profile->name, strerror(errno));
    return -1;
}

static int reset_locked(const char *profile_name)
{
    struct profile profile;
    char failure[PROFILE_FAILURE_CAP];
    if (profile_read_existing_explained(&profile, profile_name,
                                        failure) != 0) {
        logerr("%s", failure);
        return 1;
    }
    int mutation = profile_mutation_lock(&profile);
    if (mutation < 0) {
        logerr("another %s profile mutation is running", profile.name);
        return 1;
    }
    if (operation_begin(&profile, "vm reset") != 0) {
        profile_mutation_unlock(mutation);
        return 1;
    }
    (void)operation_phase("stopping");
    /* The disk is deleted only under a VM known to be stopped. A stop that
     * fails leaves the VM's state unknown, as vm stop does. */
    if (vm_stop(&profile, NULL) != VM_STOP_OK) {
        profile_mutation_unlock(mutation);
        return operation_finish(1, 0);
    }
    if (vm_process_probe(&profile, NULL) != VM_PROCESS_STALE) {
        logerr("the VM of profile %s is not known to be stopped; its disk "
               "was kept", profile.name);
        profile_mutation_unlock(mutation);
        return operation_finish(1, 0);
    }
    (void)operation_phase("removing-disk");
    int removed_disk = 0;
    int rc = remove_disk(&profile, &removed_disk);
    profile_mutation_unlock(mutation);
    /* Either way the VM is stopped and the files left are known. */
    if (rc != 0)
        return operation_finish(1, 1);
    if (removed_disk)
        logmsg("deleted the VM disk of profile %s; the next start creates a "
               "new one from the signed guest image", profile.name);
    else
        logmsg("profile %s had no VM disk; the next start creates one from "
               "the signed guest image", profile.name);
    return operation_finish(0, 1);
}

int hamn_control_reset(const char *profile_name)
{
    if (!profile_name_valid(profile_name))
        return 2;
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0) {
        logerr("cannot lock the %s profile lifecycle", profile_name);
        return 1;
    }
    int rc = reset_locked(profile_name);
    vm_lifecycle_lock_release(&lock);
    return rc;
}
