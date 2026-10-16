#include "core/guest_panic.h"

#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "util/fs.h"

#define RECORD_CAP 1024
#define OBSERVED_AT_MAX 9007199254740991.0 /* 2^53 - 1: exact in JSON */

static int printable_ascii(const char *text)
{
    for (const unsigned char *p = (const unsigned char *)text; *p; p++) {
        if (*p < 0x20 || *p >= 0x7f)
            return 0;
    }
    return 1;
}

int guest_panic_record(const char *path, const char *detail,
                       long long observed_at)
{
    if (!path || !detail || strlen(detail) >= GUEST_PANIC_DETAIL_CAP ||
        !printable_ascii(detail) || observed_at < 0 ||
        (double)observed_at > OBSERVED_AT_MAX) {
        errno = EINVAL;
        return -1;
    }
    cJSON *record = cJSON_CreateObject();
    if (!record || !cJSON_AddNumberToObject(record, "schemaVersion", 1) ||
        !cJSON_AddStringToObject(record, "detail", detail) ||
        !cJSON_AddNumberToObject(record, "observedAt", (double)observed_at)) {
        cJSON_Delete(record);
        errno = ENOMEM;
        return -1;
    }
    char *text = cJSON_PrintUnformatted(record);
    cJSON_Delete(record);
    if (!text) {
        errno = ENOMEM;
        return -1;
    }
    int rc = strlen(text) < RECORD_CAP ?
        fs_write_file_atomic(path, text, strlen(text), 0600) :
        (errno = EOVERFLOW, -1);
    int saved = errno;
    cJSON_free(text);
    errno = saved;
    return rc;
}

int guest_panic_forget(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, GUEST_PANIC_RECORD_NAME, path, sizeof(path)))
        return -1;
    return fs_unlink_if_exists(path);
}

static cJSON *unknown_snapshot(const char *detail)
{
    cJSON *value = cJSON_CreateObject();
    if (!value || !cJSON_AddStringToObject(value, "state", "unknown") ||
        !cJSON_AddStringToObject(value, "detail", detail) ||
        !cJSON_AddNullToObject(value, "observedAt")) {
        cJSON_Delete(value);
        return NULL;
    }
    return value;
}

/* The record's JSON, validated: exactly the writer's three keys, with their
 * types and bounds. NULL with errno (ENOENT when there is no record). */
static cJSON *record_read(const char *path)
{
    char text[RECORD_CAP];
    int fd = open(path, O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0)
        return NULL;
    struct stat status;
    int valid = fstat(fd, &status) == 0 && S_ISREG(status.st_mode) &&
        status.st_uid == geteuid() && status.st_nlink == 1 &&
        !(status.st_mode & 0077) && status.st_size > 0 &&
        status.st_size < (off_t)sizeof(text);
    ssize_t count = valid ? read(fd, text, sizeof(text) - 1) : -1;
    close(fd);
    if (count < 0 || count != status.st_size) {
        errno = EINVAL;
        return NULL;
    }
    text[count] = '\0';
    cJSON *value = cJSON_ParseWithLengthOpts(text, (size_t)count + 1, NULL, 1);
    const cJSON *version = cJSON_GetObjectItemCaseSensitive(value, "schemaVersion");
    const cJSON *detail = cJSON_GetObjectItemCaseSensitive(value, "detail");
    const cJSON *observed = cJSON_GetObjectItemCaseSensitive(value, "observedAt");
    if (!cJSON_IsObject(value) || cJSON_GetArraySize(value) != 3 ||
        !cJSON_IsNumber(version) || version->valuedouble != 1 ||
        !cJSON_IsString(detail) ||
        strlen(detail->valuestring) >= GUEST_PANIC_DETAIL_CAP ||
        !printable_ascii(detail->valuestring) ||
        !cJSON_IsNumber(observed) || observed->valuedouble < 0 ||
        observed->valuedouble > OBSERVED_AT_MAX ||
        floor(observed->valuedouble) != observed->valuedouble) {
        cJSON_Delete(value);
        errno = EINVAL;
        return NULL;
    }
    return value;
}

cJSON *guest_panic_snapshot(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP], detail[GUEST_PANIC_DETAIL_CAP];
    if (!profile_path(profile, GUEST_PANIC_RECORD_NAME, path, sizeof(path)))
        return unknown_snapshot("cannot name " GUEST_PANIC_RECORD_NAME);
    cJSON *record = record_read(path);
    if (!record) {
        if (errno == ENOENT)
            return cJSON_CreateNull();
        snprintf(detail, sizeof(detail), "cannot validate %s: %s",
                 GUEST_PANIC_RECORD_NAME, strerror(errno));
        return unknown_snapshot(detail);
    }
    cJSON_DeleteItemFromObjectCaseSensitive(record, "schemaVersion");
    if (!cJSON_AddStringToObject(record, "state", "panicked")) {
        cJSON_Delete(record);
        return NULL;
    }
    return record;
}
