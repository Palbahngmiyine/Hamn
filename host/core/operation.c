#include "core/operation.h"
#include "core/log.h"
#include "util/fs.h"
#include "util/proc.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static struct profile owner;
static cJSON *record;
static int persistence_failed;

static int save_record(void)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(&owner, "operation.json", path, sizeof(path))) return -1;
    char *text = cJSON_PrintUnformatted(record);
    if (!text) return -1;
    int rc = fs_write_file_atomic(path, text, strlen(text), 0600);
    free(text);
    if (rc != 0) persistence_failed = 1;
    return rc;
}

static void field(const char *name, const char *value)
{
    cJSON_DeleteItemFromObjectCaseSensitive(record, name);
    if (!cJSON_AddStringToObject(record, name, value)) abort();
}

cJSON *operation_snapshot(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP], text[16384];
    if (!profile_path(profile, "operation.json", path, sizeof(path))) return NULL;
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0 && errno == ENOENT) return cJSON_CreateNull();
    if (fd < 0) return NULL;
    struct stat st;
    int valid = fstat(fd, &st) == 0 && S_ISREG(st.st_mode) &&
        st.st_uid == geteuid() && st.st_nlink == 1 && !(st.st_mode & 0077) &&
        st.st_size > 0 && st.st_size < (off_t)sizeof(text);
    ssize_t count = valid ? read(fd, text, sizeof(text) - 1) : -1;
    close(fd);
    if (count < 0 || count != st.st_size) { errno = EINVAL; return NULL; }
    text[count] = '\0';
    cJSON *value = cJSON_Parse(text);
    if (!cJSON_IsObject(value)) { cJSON_Delete(value); errno = EINVAL; return NULL; }
    const cJSON *status = cJSON_GetObjectItemCaseSensitive(value, "status");
    const cJSON *pid = cJSON_GetObjectItemCaseSensitive(value, "pid");
    const cJSON *sec = cJSON_GetObjectItemCaseSensitive(value, "startSec");
    const cJSON *usec = cJSON_GetObjectItemCaseSensitive(value, "startUsec");
    const cJSON *uuid = cJSON_GetObjectItemCaseSensitive(value, "executableUuid");
    if (!cJSON_IsString(status) || !cJSON_IsNumber(pid) || !cJSON_IsNumber(sec) ||
        !cJSON_IsNumber(usec) || !cJSON_IsString(uuid) || pid->valuedouble < 1 ||
        pid->valuedouble > INT_MAX || sec->valuedouble < 0 || usec->valuedouble < 0) {
        cJSON_Delete(value); errno = EINVAL; return NULL;
    }
    if (strcmp(status->valuestring, "running") == 0) {
        uint64_t actual_sec, actual_usec;
        unsigned char actual_uuid[16];
        char hex[33];
        int live = proc_start_identity(pid->valueint, &actual_sec, &actual_usec) == 0 &&
            proc_executable_identity(pid->valueint, actual_uuid) == 0;
        if (live) proc_executable_uuid_format(actual_uuid, hex);
        if (!live || actual_sec != (uint64_t)sec->valuedouble ||
            actual_usec != (uint64_t)usec->valuedouble || strcmp(hex, uuid->valuestring)) {
            cJSON_SetValuestring((cJSON *)status, "outcomeUnknown");
            cJSON_AddBoolToObject(value, "recoveryRequired", 1);
        }
    }
    return value;
}

int operation_begin(const struct profile *profile, const char *name)
{
    if (record) { errno = EBUSY; return -1; }
    cJSON *previous = operation_snapshot(profile);
    if (!previous) { logerr("cannot validate previous operation record"); return -1; }
    cJSON *status = cJSON_GetObjectItemCaseSensitive(previous, "status");
    int active = cJSON_IsString(status) && !strcmp(status->valuestring, "running");
    cJSON_Delete(previous);
    if (active) { errno = EBUSY; return -1; }
    uint64_t sec, usec;
    unsigned char uuid[16], random[16];
    char hex[33], id[33];
    if (proc_start_identity(getpid(), &sec, &usec) != 0 ||
        proc_executable_identity(getpid(), uuid) != 0) return -1;
    proc_executable_uuid_format(uuid, hex);
    arc4random_buf(random, sizeof(random));
    for (int i = 0; i < 16; i++) snprintf(id + 2 * i, 3, "%02x", random[i]);
    owner = *profile;
    persistence_failed = 0;
    record = cJSON_CreateObject();
    if (!record) return -1;
    field("operationId", id);
    field("operation", name);
    field("status", "running");
    field("phase", "preparing");
    field("executableUuid", hex);
    if (!cJSON_AddNumberToObject(record, "schemaVersion", 1) ||
        !cJSON_AddNumberToObject(record, "pid", getpid()) ||
        !cJSON_AddNumberToObject(record, "startSec", (double)sec) ||
        !cJSON_AddNumberToObject(record, "startUsec", (double)usec) ||
        !cJSON_AddBoolToObject(record, "startedVm", 0)) abort();
    return save_record();
}

int operation_phase(const char *phase)
{
    if (!record) return 0;
    field("phase", phase);
    logmsg("operation %s: %s", cJSON_GetObjectItem(record, "operationId")->valuestring, phase);
    return save_record();
}

void operation_started_vm(void)
{
    if (record) {
        cJSON_DeleteItemFromObject(record, "startedVm");
        cJSON_AddBoolToObject(record, "startedVm", 1);
        if (save_record() != 0) logerr("cannot persist VM operation ownership");
    }
}

int operation_finish(int result, int restored)
{
    if (!record) return result;
    if (result != 0 && restored && proc_cancelled()) result = 130;
    field("status", result == 0 ? "completed" : !restored ? "outcomeUnknown" :
          result == 130 ? "cancelled" : "failed");
    field("error", result == 0 ? "" : log_last_error());
    cJSON_AddNumberToObject(record, "exitCode", result);
    int saved = save_record();
    cJSON_Delete(record);
    record = NULL;
    return saved || persistence_failed ? -1 : result;
}
