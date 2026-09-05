#include "core/control.h"

#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/guest_status.h"
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
    while ((entry = readdir(directory))) {
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
