#include "core/guest_filesystem.h"

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <math.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "sshmgr/ssh.h"
#include "util/fs.h"
#include "util/proc.h"

#define RECORD_NAME "guest-filesystem.json"
#define RECORD_CAP 4096
#define PROBE_OUTPUT_CAP 4096
#define EIO_SUFFIX ": Input/output error"
#define WORD_CAP 256

static void set_detail(struct guest_filesystem_check *check,
                       enum guest_filesystem_state state,
                       const char *format, ...)
    __attribute__((format(printf, 3, 4)));

static void set_detail(struct guest_filesystem_check *check,
                       enum guest_filesystem_state state,
                       const char *format, ...)
{
    check->state = state;
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(check->detail, sizeof(check->detail), format, arguments);
    va_end(arguments);
    /* Guest text reaches logs and JSON: keep it one printable line. */
    for (char *cursor = check->detail; *cursor; cursor++) {
        unsigned char ch = (unsigned char)*cursor;
        if (ch < 0x20 || ch == 0x7f)
            *cursor = '?';
    }
}

/* The next line of *cursor, without its end; NULL after the last, which
 * line ends alone do not follow. */
static const char *next_line(const char **cursor, size_t *length)
{
    const char *start = *cursor;
    if (strspn(start, "\r\n") == strlen(start))
        return NULL;
    const char *end = strchr(start, '\n');
    *length = end ? (size_t)(end - start) : strlen(start);
    *cursor = end ? end + 1 : start + *length;
    return start;
}

static int ends_with_eio(const char *line, size_t length)
{
    size_t suffix = sizeof(EIO_SUFFIX) - 1;
    return length > suffix &&
           memcmp(line + length - suffix, EIO_SUFFIX, suffix) == 0;
}

/* Splits line (length bytes) into exactly count space-separated words of
 * at most cap - 1 bytes each. */
static int split_words(const char *line, size_t length, size_t count,
                       char words[][WORD_CAP], size_t cap)
{
    size_t word = 0, used = 0;
    for (size_t index = 0; index <= length; index++) {
        char ch = index < length ? line[index] : ' ';
        if (ch != ' ') {
            if (word >= count || used + 1 >= cap)
                return -1;
            words[word][used++] = ch;
            continue;
        }
        if (used == 0)
            return -1;
        words[word++][used] = '\0';
        used = 0;
    }
    return word == count ? 0 : -1;
}

static int parse_decimal(const char *text, uint64_t maximum, uint64_t *value)
{
    if (!text[0] || strspn(text, "0123456789") != strlen(text) ||
        (text[0] == '0' && text[1]))
        return -1;
    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(text, &end, 10);
    if (errno || *end || parsed > maximum)
        return -1;
    *value = parsed;
    return 0;
}

/* "<major>:<minor>", both decimal. */
static int device_valid(const char *text)
{
    const char *colon = strchr(text, ':');
    if (!colon || colon == text || !colon[1] ||
        strspn(text, "0123456789") != (size_t)(colon - text) ||
        strspn(colon + 1, "0123456789") != strlen(colon + 1))
        return 0;
    return 1;
}

/* Mount options begin with "ro" or "rw". Returns 1 for ro, 0 for rw, -1
 * for anything else. */
static int options_read_only(const char *options)
{
    size_t first = strcspn(options, ",");
    if (first == 2 && strncmp(options, "ro", 2) == 0)
        return 1;
    if (first == 2 && strncmp(options, "rw", 2) == 0)
        return 0;
    return -1;
}

static void format_time(uint64_t seconds, char *text, size_t capacity)
{
    struct tm utc;
    time_t value = (time_t)seconds;
    if (seconds == 0 || seconds > (uint64_t)INT64_MAX ||
        !gmtime_r(&value, &utc) ||
        strftime(text, capacity, "%Y-%m-%dT%H:%M:%SZ", &utc) == 0)
        snprintf(text, capacity, "an unrecorded time");
}

void guest_filesystem_classify(int rc, const char *output, int timed_out,
                               int cancelled,
                               struct guest_filesystem_check *check)
{
    memset(check, 0, sizeof(*check));
    if (!output)
        output = "";
    if (timed_out) {
        set_detail(check, GUEST_FILESYSTEM_UNKNOWN,
                   "the check did not finish before its deadline");
        return;
    }
    if (cancelled) {
        set_detail(check, GUEST_FILESYSTEM_UNKNOWN, "the check was cancelled");
        return;
    }
    const char *cursor = output, *line;
    size_t length = 0;
    if (rc >= 0 && rc <= 254) {
        while ((line = next_line(&cursor, &length))) {
            if (ends_with_eio(line, length)) {
                set_detail(check, GUEST_FILESYSTEM_UNREADABLE,
                           "the guest cannot read its disk: %.*s",
                           (int)length, line);
                return;
            }
        }
    }
    cursor = output;
    const char *first = next_line(&cursor, &length);
    int first_length = first ? (int)length : 0;
    if (rc != 0) {
        set_detail(check, GUEST_FILESYSTEM_UNKNOWN,
                   rc < 0 ? "ssh could not be run%s%.*s" :
                   rc == 255 ? "ssh failed%s%.*s" :
                   "the check failed%s%.*s",
                   first ? ": " : "", first_length, first ? first : "");
        if (rc > 0 && rc != 255) {
            char status[32];
            snprintf(status, sizeof(status), " (exit %d)", rc);
            size_t used = strlen(check->detail);
            snprintf(check->detail + used, sizeof(check->detail) - used,
                     "%s", status);
        }
        return;
    }

    char words[4][WORD_CAP];
    if (!first || first_length < 5 || strncmp(first, "root ", 5) != 0 ||
        split_words(first + 5, (size_t)first_length - 5, 4, words,
                    sizeof(words[0])) != 0 ||
        !device_valid(words[0]) || options_read_only(words[2]) < 0 ||
        options_read_only(words[3]) < 0) {
        set_detail(check, GUEST_FILESYSTEM_UNKNOWN,
                   "unexpected check output: %.*s", first ? first_length : 6,
                   first ? first : "(none)");
        return;
    }
    int read_only = options_read_only(words[2]) || options_read_only(words[3]);
    char type[WORD_CAP], super_options[WORD_CAP];
    snprintf(type, sizeof(type), "%s", words[1]);
    snprintf(super_options, sizeof(super_options), "%s", words[3]);
    int ext4 = strcmp(type, "ext4") == 0;
    uint64_t count = 0, first_time = 0, last_time = 0;
    if (ext4) {
        size_t errors_length = 0;
        const char *errors = next_line(&cursor, &errors_length);
        if (!errors || errors_length < 7 ||
            strncmp(errors, "errors ", 7) != 0 ||
            split_words(errors + 7, errors_length - 7, 3, words,
                        sizeof(words[0])) != 0 ||
            parse_decimal(words[0], UINT32_MAX, &count) != 0 ||
            parse_decimal(words[1], INT64_MAX, &first_time) != 0 ||
            parse_decimal(words[2], INT64_MAX, &last_time) != 0) {
            set_detail(check, GUEST_FILESYSTEM_UNKNOWN,
                       "unexpected check output: %.*s",
                       errors ? (int)errors_length : 6,
                       errors ? errors : "(none)");
            return;
        }
    }
    size_t extra_length = 0;
    const char *extra = next_line(&cursor, &extra_length);
    if (extra) {
        set_detail(check, GUEST_FILESYSTEM_UNKNOWN,
                   "unexpected check output: %.*s", (int)extra_length, extra);
        return;
    }
    if (count > 0) {
        char first_text[32], last_text[32];
        format_time(first_time, first_text, sizeof(first_text));
        format_time(last_time, last_text, sizeof(last_text));
        set_detail(check, GUEST_FILESYSTEM_ERRORS_RECORDED,
                   "ext4 recorded %" PRIu64 " error%s since its last complete "
                   "check, the first at %s and the last at %s", count,
                   count == 1 ? "" : "s", first_text, last_text);
        return;
    }
    if (read_only) {
        set_detail(check, GUEST_FILESYSTEM_READ_ONLY,
                   "the root filesystem is mounted read-only (%s)",
                   super_options);
        return;
    }
    if (ext4)
        set_detail(check, GUEST_FILESYSTEM_HEALTHY, "ext4 records no errors");
    else
        set_detail(check, GUEST_FILESYSTEM_HEALTHY,
                   "the root filesystem is %s, which keeps no ext4 error "
                   "record", type);
}

int guest_filesystem_damaged(const struct guest_filesystem_check *check)
{
    return check->state == GUEST_FILESYSTEM_ERRORS_RECORDED ||
           check->state == GUEST_FILESYSTEM_UNREADABLE ||
           check->state == GUEST_FILESYSTEM_READ_ONLY;
}

void guest_filesystem_probe(const struct profile *profile, const char *ip,
                            unsigned timeout_ms,
                            struct guest_filesystem_check *check)
{
    const char *command[] = {
        "bash", "-c", GUEST_FILESYSTEM_PROBE_SCRIPT, "--",
        "/proc/self/mountinfo", "/sys/dev/block", "/sys/fs/ext4", NULL
    };
    char output[PROBE_OUTPUT_CAP];
    int rc = ssh_exec_capture_bounded(profile, ip, command, output,
                                      sizeof(output), timeout_ms);
    guest_filesystem_classify(rc, output, rc == PROC_RUN_TIMEOUT,
                              rc == 130 && proc_cancelled(), check);
}

static const char *state_name(enum guest_filesystem_state state)
{
    switch (state) {
    case GUEST_FILESYSTEM_ERRORS_RECORDED: return "errorsRecorded";
    case GUEST_FILESYSTEM_UNREADABLE: return "unreadable";
    case GUEST_FILESYSTEM_READ_ONLY: return "readOnly";
    default: return NULL;
    }
}

/* "<device>:<inode>" of disk.img, without following a symlink. Returns 0,
 * or -1 with errno (ENOENT when there is no disk). */
static int disk_identity(const struct profile *profile, char *text,
                         size_t capacity)
{
    char path[PROFILE_PATH_CAP];
    struct stat status;
    if (!profile_path(profile, "disk.img", path, sizeof(path)))
        return -1;
    if (lstat(path, &status) != 0)
        return -1;
    int length = snprintf(text, capacity, "%" PRIu64 ":%" PRIu64,
                          (uint64_t)status.st_dev, (uint64_t)status.st_ino);
    if (length < 0 || (size_t)length >= capacity) {
        errno = EOVERFLOW;
        return -1;
    }
    return 0;
}

int guest_filesystem_record(const struct profile *profile,
                            const struct guest_filesystem_check *check)
{
    const char *state = state_name(check->state);
    if (!state) {
        errno = EINVAL;
        return -1;
    }
    char disk[64], path[PROFILE_PATH_CAP];
    if (disk_identity(profile, disk, sizeof(disk)) != 0 ||
        !profile_path(profile, RECORD_NAME, path, sizeof(path)))
        return -1;
    cJSON *record = cJSON_CreateObject();
    if (!record || !cJSON_AddNumberToObject(record, "schemaVersion", 1) ||
        !cJSON_AddStringToObject(record, "state", state) ||
        !cJSON_AddStringToObject(record, "detail", check->detail) ||
        !cJSON_AddNumberToObject(record, "observedAt", (double)time(NULL)) ||
        !cJSON_AddStringToObject(record, "disk", disk)) {
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

int guest_filesystem_forget(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, RECORD_NAME, path, sizeof(path)))
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

/* The record's JSON, validated: exactly the five keys of the writer, with
 * their types. NULL with errno for any violation. */
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
    const cJSON *state = cJSON_GetObjectItemCaseSensitive(value, "state");
    const cJSON *detail = cJSON_GetObjectItemCaseSensitive(value, "detail");
    const cJSON *observed = cJSON_GetObjectItemCaseSensitive(value, "observedAt");
    const cJSON *disk = cJSON_GetObjectItemCaseSensitive(value, "disk");
    if (!cJSON_IsObject(value) || cJSON_GetArraySize(value) != 5 ||
        !cJSON_IsNumber(version) || version->valuedouble != 1 ||
        !cJSON_IsString(state) ||
        (strcmp(state->valuestring, "errorsRecorded") &&
         strcmp(state->valuestring, "unreadable") &&
         strcmp(state->valuestring, "readOnly")) ||
        !cJSON_IsString(detail) ||
        strlen(detail->valuestring) >= GUEST_FILESYSTEM_DETAIL_CAP ||
        !cJSON_IsNumber(observed) || observed->valuedouble < 0 ||
        observed->valuedouble > 9007199254740991.0 ||
        floor(observed->valuedouble) != observed->valuedouble ||
        !cJSON_IsString(disk)) {
        cJSON_Delete(value);
        errno = EINVAL;
        return NULL;
    }
    return value;
}

cJSON *guest_filesystem_snapshot(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP], detail[GUEST_FILESYSTEM_DETAIL_CAP];
    if (!profile_path(profile, RECORD_NAME, path, sizeof(path)))
        return unknown_snapshot("cannot name " RECORD_NAME);
    cJSON *record = record_read(path);
    if (!record) {
        if (errno == ENOENT)
            return cJSON_CreateNull();
        snprintf(detail, sizeof(detail), "cannot validate %s: %s",
                 RECORD_NAME, strerror(errno));
        return unknown_snapshot(detail);
    }
    char disk[64];
    if (disk_identity(profile, disk, sizeof(disk)) != 0) {
        int error = errno;
        cJSON_Delete(record);
        if (error == ENOENT)
            return cJSON_CreateNull();
        snprintf(detail, sizeof(detail), "cannot examine disk.img: %s",
                 strerror(error));
        return unknown_snapshot(detail);
    }
    /* The record describes the disk it was observed on; a disk.img that
     * replaced it has not been checked. */
    if (strcmp(cJSON_GetObjectItemCaseSensitive(record, "disk")->valuestring,
               disk) != 0) {
        cJSON_Delete(record);
        return cJSON_CreateNull();
    }
    cJSON_DeleteItemFromObjectCaseSensitive(record, "schemaVersion");
    cJSON_DeleteItemFromObjectCaseSensitive(record, "disk");
    return record;
}
