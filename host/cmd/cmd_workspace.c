/* vm up and vm down: the VM of a workspace (core/workspace.h). Each call
 * holds the lifecycle lock of the workspace's profile from the moment it
 * reads the profile until its start, stop or purge ends, so that a creation
 * and the deletion of a profile that could not start are one step that no
 * other operation on that profile can come between. Admission belongs to
 * the start alone; the lock order of core/admission.h holds because no
 * profile lock is taken while the start holds the admission lock. */

#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#include "cjson/cJSON.h"
#include "core/control.h"
#include "core/control_locked.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/profile.h"
#include "core/purge.h"
#include "core/workspace.h"
#include "util/proc.h"

/* {"profile","workspace"}, or NULL when out of memory. */
static char *identified(const struct workspace *workspace)
{
    cJSON *value = cJSON_CreateObject();
    char *text = NULL;
    if (value &&
        cJSON_AddStringToObject(value, "profile", workspace->profile) &&
        cJSON_AddStringToObject(value, "workspace", workspace->path))
        text = cJSON_PrintUnformatted(value);
    cJSON_Delete(value);
    if (!text)
        logerr("cannot build the result for workspace %s", workspace->path);
    return text;
}

/* Whether the profile with this record serves workspace; logs why not. */
static int serves(const struct profile *profile,
                  const struct workspace *recorded,
                  const struct workspace *workspace)
{
    if (workspace_same(recorded, workspace))
        return 1;
    if (strcmp(recorded->path, workspace->path) == 0)
        logerr("profile %s belongs to an earlier directory at %s: its "
               "device or inode differs, as when the directory was removed "
               "and made again or its volume was attached again. vm up and "
               "vm down leave it alone; vm delete --profile %s --purge --yes "
               "deletes it with its disk", profile->name, workspace->path,
               profile->name);
    else
        logerr("profile %s belongs to workspace %s, not to %s",
               profile->name, recorded->path, workspace->path);
    return 0;
}

/* Logs why the existing profile is not ephemeral. */
static void refuse_foreign(const struct profile *profile)
{
    logerr("profile %s exists and is not the VM of a workspace: it has no "
           "%s record. vm up and vm down leave it alone; if an interrupted vm "
           "up left it, vm delete --profile %s --purge --yes deletes it with "
           "its disk", profile->name, WORKSPACE_RECORD_FILE, profile->name);
}

/* Deletes the profile that this vm up created and could not start, so that
 * the failed call leaves no profile behind. The reason of the failure stays
 * the start's, followed by what became of the profile. */
static void discard_created(const char *name)
{
    char reason[2048];
    snprintf(reason, sizeof(reason), "%s", log_last_error());
    proc_cleanup_begin();
    int absent = 0;
    int rc = control_purge_locked(name, &absent);
    proc_cleanup_end();
    if (rc == 0) {
        log_set_error("%s; vm up deleted the profile %s it had created",
                      reason[0] ? reason : "vm up failed", name);
    } else {
        char purge[1024];
        snprintf(purge, sizeof(purge), "%s", log_last_error());
        log_set_error("%s; the profile %s that vm up created could not be "
                      "deleted: %s", reason[0] ? reason : "vm up failed",
                      name, purge);
    }
}

/* Whether stored shares a directory read-only: the git directory of a
 * profile that vm up created. */
static int shares_git(const struct profile *stored)
{
    for (size_t index = 0; index < stored->mount_count; index++) {
        if (!stored->mounts[index].writable)
            return 1;
    }
    return 0;
}

/* git_missing says why desired shares no git directory although git_mount
 * asked for one; it is empty otherwise. */
static int up_locked(const struct workspace *workspace,
                     const struct profile *desired, int git_mount,
                     const char *git_missing,
                     unsigned cpus, unsigned memory_gib, unsigned disk_gib,
                     char **refusal)
{
    struct workspace recorded;
    int found = workspace_record_read(desired, &recorded);
    if (found < 0)
        return 1;
    struct profile stored;
    if (found) {
        if (!serves(desired, &recorded, workspace))
            return 4;
        /* Without its configuration a start would take the defaults,
         * which share the whole home directory. */
        char failure[PROFILE_FAILURE_CAP];
        if (profile_read_existing_explained(&stored, desired->name,
                                            failure) != 0) {
            logerr("%s", failure);
            return 1;
        }
        /* The mounts are those of the creation. Asking for fewer than the
         * profile has cannot be met without deleting it. */
        if (!git_mount && shares_git(&stored)) {
            logerr("profile %s shares the git directory of %s; to start it "
                   "without, delete it first with vm down --workspace %s "
                   "--purge --yes", desired->name, workspace->path,
                   workspace->path);
            return 4;
        }
        return control_start_locked(desired->name, cpus, memory_gib,
                                    disk_gib, refusal);
    }
    if (profile_read_existing(&stored, desired->name) == 0) {
        refuse_foreign(desired);
        return 4;
    }
    int rc = control_create_locked(desired);
    /* 5: config.yaml can hold the settings of a profile without a record. */
    if (rc == 5) {
        discard_created(desired->name);
        return 1;
    }
    if (rc != 0)
        return rc;
    if (git_missing[0])
        logmsg("warning: the VM does not share a git directory: %s",
               git_missing);
    if (desired->mount_count > 1)
        logmsg("profile %s shares the workspace %s writable and the git "
               "directory %s read-only, each at its own path", desired->name,
               desired->mounts[0].location, desired->mounts[1].location);
    else
        logmsg("profile %s shares the workspace %s writable at its own path",
               desired->name, desired->mounts[0].location);
    if (workspace_record_write(desired, workspace) != 0) {
        logerr("cannot record workspace %s in profile %s: %s",
               workspace->path, desired->name, strerror(errno));
        rc = 1;
    } else {
        rc = control_start_locked(desired->name, cpus, memory_gib, disk_gib,
                                  refusal);
    }
    if (rc != 0)
        discard_created(desired->name);
    return rc;
}

int hamn_control_up(const char *path, unsigned cpus, unsigned memory_gib,
                    unsigned disk_gib, int git_mount, char **result)
{
    if (!result)
        return 2;
    *result = NULL;
    if ((git_mount != 0 && git_mount != 1) || memory_gib > UINT_MAX / 1024U) {
        logerr("invalid VM resource request");
        return 2;
    }
    struct workspace workspace;
    if (workspace_identify(path, &workspace) != 0)
        return 2;
    *result = identified(&workspace);
    if (!*result)
        return 1;
    char git[PATH_MAX] = "";
    char git_missing[WORKSPACE_REASON_CAP] = "";
    if (git_mount)
        (void)workspace_git_directory(workspace.path, git, git_missing);
    struct profile desired;
    workspace_profile(&workspace, git, &desired);
    /* A new profile is created with the requested resources: its start
     * would refuse a disk below the default one as a shrink. */
    if (cpus)
        desired.cpus = cpus;
    if (memory_gib)
        desired.mem_mib = memory_gib * 1024;
    if (disk_gib)
        desired.disk_gib = disk_gib;
    if (profile_locate(&desired) != 0) {
        logerr("cannot locate profile %s: %s", desired.name, strerror(errno));
        return 1;
    }
    if (!profile_storable(&desired)) {
        logerr("workspace %s cannot be shared: a profile cannot store its "
               "path%s", workspace.path, git[0] ? " or that of its git "
               "directory" : "");
        return 2;
    }
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(desired.name, &lock) != 0) {
        logerr("cannot lock the %s profile lifecycle", desired.name);
        return 1;
    }
    char *refusal = NULL;
    int rc = 130;
    /* The wait for the lock cannot be interrupted, and a call cancelled
     * meanwhile must not create or start anything. */
    if (proc_cancelled())
        logerr("cancelled before any change");
    else
        rc = up_locked(&workspace, &desired, git_mount, git_missing, cpus,
                       memory_gib, disk_gib, &refusal);
    vm_lifecycle_lock_release(&lock);
    if (rc == HAMN_CONTROL_ADMISSION_REFUSED) {
        hamn_control_free(*result);
        *result = refusal;
    }
    return rc;
}

/* A workspace without a profile: nothing to purge, and nothing to stop. */
static int no_profile(const struct workspace *workspace,
                      const struct profile *profile, int purge, int *absent)
{
    if (!purge) {
        logerr("workspace %s has no VM: profile %s does not exist",
               workspace->path, profile->name);
        return 1;
    }
    *absent = 1;
    logmsg("workspace %s has no VM; nothing to delete", workspace->path);
    return 0;
}

static int down_locked(const struct workspace *workspace,
                       const struct profile *profile, int purge, int *absent)
{
    struct workspace recorded;
    int found = workspace_record_read(profile, &recorded);
    if (found < 0)
        return 1;
    if (!found) {
        struct stat status;
        if (lstat(profile->dir, &status) == 0) {
            refuse_foreign(profile);
            return 4;
        }
        if (errno != ENOENT) {
            logerr("cannot look for profile %s: %s", profile->name,
                   strerror(errno));
            return 1;
        }
        /* Deleted by another call while this one waited for the lock. */
        return no_profile(workspace, profile, purge, absent);
    }
    if (!serves(profile, &recorded, workspace))
        return 4;
    return purge ? control_purge_locked(profile->name, absent) :
        control_stop_locked(profile->name);
}

int hamn_control_down(const char *path, int purge, int *absent,
                      char **result)
{
    if (!absent || !result)
        return 2;
    *absent = 0;
    *result = NULL;
    if (purge != 0 && purge != 1)
        return 2;
    struct workspace workspace;
    if (workspace_identify(path, &workspace) != 0)
        return 2;
    *result = identified(&workspace);
    if (!*result)
        return 1;
    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.name, sizeof(profile.name), "%s", workspace.profile);
    if (profile_locate(&profile) != 0) {
        logerr("cannot locate profile %s: %s", profile.name, strerror(errno));
        return 1;
    }
    if (purge)
        purge_sweep();
    /* A workspace without a profile gets no lock file or ~/.hamn. */
    struct stat status;
    if (lstat(profile.dir, &status) != 0) {
        if (errno != ENOENT) {
            logerr("cannot look for profile %s: %s", profile.name,
                   strerror(errno));
            return 1;
        }
        return no_profile(&workspace, &profile, purge, absent);
    }
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile.name, &lock) != 0) {
        logerr("cannot lock the %s profile lifecycle", profile.name);
        return 1;
    }
    int rc = down_locked(&workspace, &profile, purge, absent);
    vm_lifecycle_lock_release(&lock);
    return rc;
}
