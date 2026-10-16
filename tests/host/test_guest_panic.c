/* The record of the guest kernel panic that ended a VM's last run
 * (core/guest_panic.h): what vmrun writes, what `vm status` and the profile
 * list report from it, and how records the writer cannot have written are
 * reported. Runs in a fixture HOME under /tmp; no VM is involved. */
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/control.h"
#include "core/guest_panic.h"
#include "core/profile.h"

static char base[PATH_MAX];

static void fail(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    fputs("FAIL: ", stderr);
    vfprintf(stderr, format, arguments);
    fputc('\n', stderr);
    va_end(arguments);
    exit(1);
}

#define REQUIRE(condition, ...) \
    do { if (!(condition)) fail(__VA_ARGS__); } while (0)

static void write_file(const char *path, const char *text, mode_t mode)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, mode);
    REQUIRE(fd >= 0, "open %s: %s", path, strerror(errno));
    size_t length = strlen(text);
    REQUIRE(write(fd, text, length) == (ssize_t)length && close(fd) == 0,
            "write %s", path);
    REQUIRE(chmod(path, mode) == 0, "chmod %s", path);
}

static char *read_file(const char *path)
{
    static char text[2048];
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    REQUIRE(fd >= 0, "open %s: %s", path, strerror(errno));
    ssize_t length = read(fd, text, sizeof(text) - 1);
    REQUIRE(length >= 0 && close(fd) == 0, "read %s", path);
    text[length] = '\0';
    return text;
}

static cJSON *snapshot_of(const struct profile *profile)
{
    cJSON *value = guest_panic_snapshot(profile);
    REQUIRE(value, "snapshot ran out of memory");
    return value;
}

static void require_no_record(const struct profile *profile, const char *why)
{
    cJSON *value = snapshot_of(profile);
    REQUIRE(cJSON_IsNull(value), "%s: %s", why, cJSON_PrintUnformatted(value));
    cJSON_Delete(value);
}

static void require_unknown(const struct profile *profile, const char *why)
{
    cJSON *value = snapshot_of(profile);
    const cJSON *state = cJSON_GetObjectItemCaseSensitive(value, "state");
    const cJSON *detail = cJSON_GetObjectItemCaseSensitive(value, "detail");
    REQUIRE(cJSON_IsString(state) && !strcmp(state->valuestring, "unknown") &&
            cJSON_IsString(detail) && detail->valuestring[0] &&
            cJSON_IsNull(cJSON_GetObjectItemCaseSensitive(value, "observedAt")) &&
            cJSON_GetArraySize(value) == 3,
            "%s: %s", why, cJSON_PrintUnformatted(value));
    cJSON_Delete(value);
}

static void require_panicked_object(const cJSON *value, const char *detail,
                                    double observed_at, const char *where)
{
    const cJSON *state = cJSON_GetObjectItemCaseSensitive(value, "state");
    const cJSON *text = cJSON_GetObjectItemCaseSensitive(value, "detail");
    const cJSON *observed = cJSON_GetObjectItemCaseSensitive(value, "observedAt");
    REQUIRE(cJSON_IsObject(value) && cJSON_GetArraySize(value) == 3 &&
            cJSON_IsString(state) && !strcmp(state->valuestring, "panicked") &&
            cJSON_IsString(text) && !strcmp(text->valuestring, detail) &&
            cJSON_IsNumber(observed) && observed->valuedouble == observed_at,
            "%s: %s", where, cJSON_PrintUnformatted(value));
}

static void require_record(const struct profile *profile, const char *detail,
                           double observed_at)
{
    cJSON *value = snapshot_of(profile);
    require_panicked_object(value, detail, observed_at, "snapshot");
    cJSON_Delete(value);
}

/* `vm status` and the profile list report the record as guestKernelPanic. */
static void require_reported(const char *name, const char *detail,
                             double observed_at)
{
    char *text = NULL;
    REQUIRE(hamn_control_query(name, &text) == 0, "vm status");
    cJSON *status = cJSON_Parse(text);
    hamn_control_free(text);
    const cJSON *panic = cJSON_GetObjectItemCaseSensitive(status, "guestKernelPanic");
    if (detail)
        require_panicked_object(panic, detail, observed_at, "vm status");
    else
        REQUIRE(cJSON_IsNull(panic), "vm status without a record: %s",
                cJSON_PrintUnformatted(status));
    cJSON_Delete(status);

    REQUIRE(hamn_control_query(NULL, &text) == 0, "profile list");
    cJSON *list = cJSON_Parse(text);
    hamn_control_free(text);
    REQUIRE(cJSON_IsArray(list) && cJSON_GetArraySize(list) == 1, "one row: %s",
            cJSON_PrintUnformatted(list));
    panic = cJSON_GetObjectItemCaseSensitive(cJSON_GetArrayItem(list, 0),
                                             "guestKernelPanic");
    if (detail)
        require_panicked_object(panic, detail, observed_at, "profile list");
    else
        REQUIRE(cJSON_IsNull(panic), "list row without a record: %s",
                cJSON_PrintUnformatted(list));
    cJSON_Delete(list);
}

static void the_writer_rejects_what_it_must_not_write(const char *record)
{
    char long_detail[GUEST_PANIC_DETAIL_CAP + 1];
    memset(long_detail, 'd', GUEST_PANIC_DETAIL_CAP);
    long_detail[GUEST_PANIC_DETAIL_CAP] = '\0';
    char longest[GUEST_PANIC_DETAIL_CAP];
    memset(longest, 'd', GUEST_PANIC_DETAIL_CAP - 1);
    longest[GUEST_PANIC_DETAIL_CAP - 1] = '\0';
    const struct {
        const char *path;
        const char *detail;
        long long observed_at;
    } rejected[] = {
        { NULL, "x", 1 },
        { record, NULL, 1 },
        { record, long_detail, 1 },
        { record, "tab\there", 1 },
        { record, "line\nbreak", 1 },
        { record, "caf\xc3\xa9", 1 },
        { record, "del\x7f", 1 },
        { record, "x", -1 },
        { record, "x", 9007199254740992LL },
    };
    for (size_t i = 0; i < sizeof(rejected) / sizeof(rejected[0]); i++) {
        errno = 0;
        REQUIRE(guest_panic_record(rejected[i].path, rejected[i].detail,
                                   rejected[i].observed_at) == -1 &&
                    errno == EINVAL,
                "case %zu was written or failed otherwise: %s", i,
                strerror(errno));
        REQUIRE(access(record, F_OK) != 0, "case %zu left a record", i);
    }
    /* The bounds themselves are writable. */
    REQUIRE(guest_panic_record(record, longest, 9007199254740991LL) == 0,
            "the longest detail at the largest time: %s", strerror(errno));
    REQUIRE(guest_panic_record(record, "", 0) == 0, "an empty detail at 0");
    REQUIRE(unlink(record) == 0, "unlink");
}

static void records_the_writer_cannot_have_written_are_unknown(
    const struct profile *profile, const char *record)
{
    const char *invalid[] = {
        "{",
        "[]",
        "null",
        "{\"schemaVersion\":2,\"detail\":\"x\",\"observedAt\":1}",
        "{\"schemaVersion\":1,\"detail\":1,\"observedAt\":1}",
        "{\"schemaVersion\":1,\"detail\":\"caf\\u00e9\",\"observedAt\":1}",
        "{\"schemaVersion\":1,\"detail\":\"tab\\there\",\"observedAt\":1}",
        "{\"schemaVersion\":1,\"detail\":\"x\",\"observedAt\":-1}",
        "{\"schemaVersion\":1,\"detail\":\"x\",\"observedAt\":1.5}",
        "{\"schemaVersion\":1,\"detail\":\"x\",\"observedAt\":9007199254740992}",
        "{\"schemaVersion\":1,\"detail\":\"x\",\"observedAt\":\"1\"}",
        "{\"schemaVersion\":1,\"detail\":\"x\"}",
        "{\"schemaVersion\":1,\"detail\":\"x\",\"observedAt\":1,\"extra\":1}",
        "{\"schemaVersion\":1,\"detail\":\"x\",\"detail\":\"y\"}",
        "{\"schemaVersion\":1,\"detail\":\"x\",\"observedAt\":1} trailing",
    };
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); i++) {
        write_file(record, invalid[i], 0600);
        require_unknown(profile, invalid[i]);
    }
    char text[1200];
    char long_detail[GUEST_PANIC_DETAIL_CAP + 1];
    memset(long_detail, 'd', GUEST_PANIC_DETAIL_CAP);
    long_detail[GUEST_PANIC_DETAIL_CAP] = '\0';
    snprintf(text, sizeof(text),
             "{\"schemaVersion\":1,\"detail\":\"%s\",\"observedAt\":1}",
             long_detail);
    write_file(record, text, 0600);
    require_unknown(profile, "a detail longer than the writer's");
    char padded[1100];
    memset(padded, ' ', sizeof(padded) - 1);
    padded[sizeof(padded) - 1] = '\0';
    memcpy(padded, "{\"schemaVersion\":1,\"detail\":\"x\",\"observedAt\":1}", 47);
    write_file(record, padded, 0600);
    require_unknown(profile, "a record larger than the writer's");

    REQUIRE(guest_panic_record(record, "valid", 7) == 0, "record");
    char valid[256];
    snprintf(valid, sizeof(valid), "%s", read_file(record));
    write_file(record, valid, 0644);
    require_unknown(profile, "a record readable by others");
    write_file(record, "", 0600);
    require_unknown(profile, "an empty record");
    write_file(record, valid, 0600);
    char second_name[PATH_MAX];
    snprintf(second_name, sizeof(second_name), "%s.link", record);
    REQUIRE(link(record, second_name) == 0, "hard link");
    require_unknown(profile, "a record with two names");
    REQUIRE(unlink(second_name) == 0, "unlink");
    REQUIRE(unlink(record) == 0 && symlink(second_name, record) == 0, "symlink");
    write_file(second_name, valid, 0600);
    require_unknown(profile, "a symlinked record");
    REQUIRE(unlink(record) == 0 && unlink(second_name) == 0, "unlink");
    REQUIRE(mkdir(record, 0700) == 0, "mkdir");
    require_unknown(profile, "a directory in the record's place");
    REQUIRE(rmdir(record) == 0, "rmdir");
    require_no_record(profile, "after the invalid records");
}

static void the_record_is_written_reported_and_forgotten(void)
{
    char home[PATH_MAX];
    snprintf(home, sizeof(home), "%s/home", base);
    REQUIRE(mkdir(home, 0700) == 0, "mkdir home");
    REQUIRE(setenv("HOME", home, 1) == 0, "HOME");
    REQUIRE(hamn_control_configure("fixture", 0, 0, 0, 1, -1) == 0,
            "create the profile");
    struct profile profile;
    REQUIRE(profile_read_existing(&profile, "fixture") == 0, "read profile");
    char record[PATH_MAX];
    profile_path(&profile, GUEST_PANIC_RECORD_NAME, record, sizeof(record));

    require_no_record(&profile, "no record");
    require_reported("fixture", NULL, 0);
    the_writer_rejects_what_it_must_not_write(record);

    static const char reason[] =
        "corrupted stack end detected inside scheduler";
    long long observed_at = (long long)time(NULL);
    REQUIRE(guest_panic_record(record, reason, observed_at) == 0, "record: %s",
            strerror(errno));
    struct stat status;
    REQUIRE(lstat(record, &status) == 0 && S_ISREG(status.st_mode) &&
            (status.st_mode & 0777) == 0600 && status.st_nlink == 1,
            "record mode");
    require_record(&profile, reason, (double)observed_at);
    require_reported("fixture", reason, (double)observed_at);

    /* A failed write keeps the earlier record. */
    REQUIRE(chmod(profile.dir, 0500) == 0, "chmod");
    int recorded = guest_panic_record(record, "another", observed_at + 1);
    int error = errno;
    REQUIRE(chmod(profile.dir, 0700) == 0, "chmod");
    REQUIRE(recorded == -1 && error == EACCES,
            "a write into a read-only directory: %d %s", recorded,
            strerror(error));
    require_record(&profile, reason, (double)observed_at);
    /* A later panic replaces it. */
    REQUIRE(guest_panic_record(record, "Oops: Fatal exception",
                               observed_at + 2) == 0, "rerecord");
    require_record(&profile, "Oops: Fatal exception", (double)(observed_at + 2));

    records_the_writer_cannot_have_written_are_unknown(&profile, record);

    /* forget removes the record, succeeds when there is none, and reports a
     * record it cannot remove. */
    REQUIRE(guest_panic_record(record, reason, observed_at) == 0, "record");
    REQUIRE(chmod(profile.dir, 0500) == 0, "chmod");
    int forgotten = guest_panic_forget(&profile);
    error = errno;
    REQUIRE(chmod(profile.dir, 0700) == 0, "chmod");
    REQUIRE(forgotten == -1 && error == EACCES, "forget in a read-only "
            "directory: %d %s", forgotten, strerror(error));
    require_record(&profile, reason, (double)observed_at);
    REQUIRE(guest_panic_forget(&profile) == 0, "forget");
    require_no_record(&profile, "forgotten");
    require_reported("fixture", NULL, 0);
    REQUIRE(guest_panic_forget(&profile) == 0, "forget twice");
    REQUIRE(access(record, F_OK) != 0, "forgotten record exists");
}

int main(void)
{
    char temporary[] = "/tmp/hamn-guest-panic-XXXXXX";
    REQUIRE(mkdtemp(temporary), "mkdtemp");
    REQUIRE(realpath(temporary, base), "realpath");
    the_record_is_written_reported_and_forgotten();
    char command[PATH_MAX + 32];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", base);
    REQUIRE(system(command) == 0, "cannot remove %s", base);
    puts("PASS: the guest kernel panic record is validated and reported until "
         "it is forgotten");
    return 0;
}
