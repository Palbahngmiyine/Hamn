#include "core/control.h"

#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/guest_status.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"
#include "core/state.h"

const char *vm_live_state(const struct profile *, char *, size_t);

static cJSON *profile_snapshot(const char *name)
{
    struct profile profile;
    struct vm_state state;
    if (profile_read_existing(&profile, name) != 0 ||
        state_load(&profile, &state) != 0)
        return NULL;
    char live[32], socket[PROFILE_PATH_CAP];
    vm_live_state(&profile, live, sizeof(live));
    if (!profile_path(&profile, "docker.sock", socket, sizeof(socket)))
        return NULL;
    cJSON *value = cJSON_CreateObject();
    if (!value || !cJSON_AddStringToObject(value, "name", profile.name) ||
        !cJSON_AddStringToObject(value, "state", live) ||
        !cJSON_AddStringToObject(value, "migration", profile.legacy_k3s ? "pending" : "current") ||
        !cJSON_AddStringToObject(value, "directory", profile.dir) ||
        !cJSON_AddStringToObject(value, "dockerSocket", socket) ||
        !cJSON_AddNumberToObject(value, "cpus", profile.cpus) ||
        !cJSON_AddNumberToObject(value, "memoryMiB", profile.mem_mib) ||
        !cJSON_AddNumberToObject(value, "diskGiB", profile.disk_gib) ||
        !cJSON_AddStringToObject(value, "ip", state.ip)) {
        cJSON_Delete(value);
        return NULL;
    }
    return value;
}

static cJSON *profiles_snapshot(void)
{
    char root[PROFILE_PATH_CAP];
    if (!hamn_home(root, sizeof(root)))
        return NULL;
    cJSON *values = cJSON_CreateArray();
    if (!values)
        return NULL;
    DIR *directory = opendir(root);
    if (!directory) {
        if (errno == ENOENT)
            return values;
        cJSON_Delete(values);
        return NULL;
    }
    struct dirent *entry;
    for (;;) {
        errno = 0;
        entry = readdir(directory);
        if (!entry) {
            if (errno) goto fail;
            break;
        }
        if (!profile_name_valid(entry->d_name))
            continue;
        char path[PROFILE_PATH_CAP];
        int length = snprintf(path, sizeof(path), "%s/%s/config.yaml",
                              root, entry->d_name);
        if (length < 0 || length >= (int)sizeof(path)) {
            errno = ENAMETOOLONG;
            goto fail;
        }
        if (access(path, F_OK) != 0) {
            if (errno == ENOENT)
                continue;
            goto fail;
        }
        length = snprintf(path, sizeof(path), "%s/%s/deleted", root, entry->d_name);
        if (length < 0 || length >= (int)sizeof(path)) { errno = ENAMETOOLONG; goto fail; }
        struct stat deleted;
        if (lstat(path, &deleted) == 0) {
            if (!S_ISREG(deleted.st_mode) || deleted.st_uid != geteuid()) { errno = EINVAL; goto fail; }
            continue;
        }
        if (errno != ENOENT) goto fail;
        cJSON *value = profile_snapshot(entry->d_name);
        if (!value)
            goto fail;
        if (!cJSON_AddItemToArray(values, value)) {
            cJSON_Delete(value);
            goto fail;
        }
    }
    if (closedir(directory) != 0) {
        cJSON_Delete(values);
        return NULL;
    }
    return values;
fail:
    closedir(directory);
    cJSON_Delete(values);
    return NULL;
}

int hamn_control_query(const char *profile, char **result)
{
    if (!result) {
        errno = EINVAL;
        return -1;
    }
    *result = NULL;
    cJSON *value = profile ? profile_snapshot(profile) : profiles_snapshot();
    if (!value)
        return -1;
    *result = cJSON_PrintUnformatted(value);
    cJSON_Delete(value);
    return *result ? 0 : -1;
}

void hamn_control_free(char *result)
{
    cJSON_free(result);
}

int hamn_control_configure(const char *name, unsigned cpus,
                           unsigned memory_gib, unsigned disk_gib, int create)
{
    if (!profile_name_valid(name) || memory_gib > UINT_MAX / 1024U)
        return 2;
    struct vm_lifecycle_lock lifecycle;
    if (vm_lifecycle_lock_acquire(name, &lifecycle) != 0)
        return 1;
    struct profile profile;
    int rc = 1, mutation = -1;
    if (create) {
        char root[PROFILE_PATH_CAP], path[PROFILE_PATH_CAP];
        struct stat existing;
        if (!hamn_home(root, sizeof(root))) goto out;
        int length = snprintf(path, sizeof(path), "%s/%s", root, name);
        if (length < 0 || length >= (int)sizeof(path)) goto out;
        if (lstat(path, &existing) == 0) {
            logerr("profile already exists: %s", name);
            rc = 4;
            goto out;
        }
        if (errno != ENOENT) goto out;
    }
    if ((create ? profile_load(&profile, name) :
         profile_read_existing(&profile, name)) != 0)
        goto out;
    mutation = profile_mutation_lock(&profile);
    if (mutation < 0)
        goto out;
    if (vm_process_probe(&profile, NULL) != VM_PROCESS_STALE) {
        logerr("VM must be stopped before changing settings");
        goto out;
    }
    if (cpus) profile.cpus = cpus;
    if (memory_gib) profile.mem_mib = memory_gib * 1024U;
    if (disk_gib) profile.disk_gib = disk_gib;
    if (profile_save(&profile) != 0) {
        logerr("cannot save settings: %s", strerror(errno));
        goto out;
    }
    rc = 0;
out:
    if (mutation >= 0)
        profile_mutation_unlock(mutation);
    vm_lifecycle_lock_release(&lifecycle);
    return rc;
}
