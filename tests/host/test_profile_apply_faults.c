/* hamn_control_apply() when the write of config.yaml fails, or when the
 * configuration cannot be read back afterwards. The Makefile compiles
 * host/core/apply.c into this test with its calls of profile_save and
 * profile_read_existing_reason renamed to the two functions below, which
 * fail on request and otherwise call the real ones. */
#undef profile_save
#undef profile_read_existing_reason

#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/control.h"
#include "core/log.h"
#include "core/profile.h"
#include "util/fs.h"

#define DEFINITION(name, spec) \
    "apiVersion: hamn/v1\nkind: Profile\nmetadata:\n  name: " name "\nspec:" spec

enum save_fault {
    SAVE_WORKS,
    SAVE_FAILS_BEFORE_WRITING,
    SAVE_FAILS_AFTER_WRITING,
    SAVE_WORKS_AND_NO_RESULT_CAN_BE_BUILT,
};

static enum save_fault save_fault;
static int saves;               /* calls of the save since the last reset */
static int reads_until_failure; /* 0: every read works; n: the nth fails */
/* Text that replaces config.yaml right after the next read of it, as a
 * writer would that got in while the apply held no lock. */
static const char *replacement_after_read;
static char home[64];
static char definition[96];

static const char *in_profile(const char *name, const char *file)
{
    static char path[256];
    snprintf(path, sizeof(path), "%s/.hamn/%s%s%s", home, name,
             file ? "/" : "", file ? file : "");
    return path;
}

static void *no_memory(size_t size)
{
    (void)size;
    return NULL;
}

int fault_profile_save(const struct profile *profile)
{
    saves++;
    if (save_fault == SAVE_FAILS_BEFORE_WRITING) {
        errno = EIO;
        return -1;
    }
    int rc = profile_save(profile);
    if (rc == 0 && save_fault == SAVE_FAILS_AFTER_WRITING) {
        errno = EIO;
        return -1;
    }
    if (rc == 0 && save_fault == SAVE_WORKS_AND_NO_RESULT_CAN_BE_BUILT) {
        /* The result is built with cJSON after the write. */
        cJSON_Hooks hooks = { .malloc_fn = no_memory, .free_fn = free };
        cJSON_InitHooks(&hooks);
    }
    return rc;
}

int fault_profile_read_existing_reason(struct profile *profile,
                                       const char *name,
                                       char reason[PROFILE_REASON_CAP])
{
    if (reads_until_failure > 0 && --reads_until_failure == 0) {
        snprintf(reason, PROFILE_REASON_CAP, "%s", strerror(EIO));
        errno = EIO;
        return -1;
    }
    int rc = profile_read_existing_reason(profile, name, reason);
    if (replacement_after_read) {
        int saved = errno;
        const char *text = replacement_after_read;
        replacement_after_read = NULL;
        assert(fs_write_file_atomic(in_profile(name, "config.yaml"), text,
                                    strlen(text), 0600) == 0);
        errno = saved;
    }
    return rc;
}

static int exists(const char *path)
{
    struct stat status;
    return lstat(path, &status) == 0;
}

static size_t read_file(const char *path, char *data, size_t capacity)
{
    int fd = open(path, O_RDONLY);
    assert(fd >= 0);
    ssize_t length = read(fd, data, capacity - 1);
    assert(length >= 0 && close(fd) == 0);
    data[length] = '\0';
    return (size_t)length;
}

/* One apply with the given faults, which are cleared afterwards. */
static int apply(const char *profile, const char *text, enum save_fault save,
                 int failing_read)
{
    int fd = open(definition, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    assert(fd >= 0 && write(fd, text, strlen(text)) == (ssize_t)strlen(text) &&
           close(fd) == 0);
    save_fault = save;
    reads_until_failure = failing_read;
    saves = 0;
    char *json = NULL;
    int rc = hamn_control_apply(profile, definition, 0, &json);
    save_fault = SAVE_WORKS;
    reads_until_failure = 0;
    cJSON_InitHooks(NULL);
    assert((rc == 0) == (json != NULL));
    hamn_control_free(json);
    return rc;
}

/* The result is 1 only when config.yaml is known to hold what it held. */
static void a_failed_write_of_an_existing_profile(void)
{
    const char *first = DEFINITION("work", "\n  cpus: 2\n");
    const char *second = DEFINITION("work", "\n  cpus: 4\n");
    char before[4096], after[4096];
    struct profile stored;
    assert(apply("work", first, SAVE_WORKS, 0) == 0);
    read_file(in_profile("work", "config.yaml"), before, sizeof(before));

    /* Nothing was written: unchanged, and reported as such. */
    assert(apply("work", second, SAVE_FAILS_BEFORE_WRITING, 0) == 1);
    assert(strcmp(log_last_error(),
                  "cannot save settings: Input/output error") == 0);
    read_file(in_profile("work", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0);

    /* Nothing was written, but the configuration cannot be read back to
     * show it (the third read of a change is that check): not reported as
     * unchanged. */
    assert(apply("work", second, SAVE_FAILS_BEFORE_WRITING, 3) == 5);
    assert(strstr(log_last_error(), "cannot be read back"));
    read_file(in_profile("work", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0);

    /* The configuration cannot be read before the write, without the lock
     * and again under it: refused, and both locks are released, or the next
     * call would not return. */
    assert(apply("work", second, SAVE_WORKS, 1) == 1);
    assert(strstr(log_last_error(),
                  "cannot read the configuration of profile work"));
    assert(apply("work", second, SAVE_WORKS, 2) == 1);
    read_file(in_profile("work", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0);

    /* The write failed after the file was replaced: the new settings are
     * in place, and the result says that they may be. A repeated apply
     * then has nothing to do. */
    assert(apply("work", second, SAVE_FAILS_AFTER_WRITING, 0) == 5);
    assert(strstr(log_last_error(),
                  "were written but not confirmed durable"));
    assert(profile_read_existing(&stored, "work") == 0 && stored.cpus == 4);
    read_file(in_profile("work", "config.yaml"), before, sizeof(before));
    assert(apply("work", second, SAVE_FAILS_BEFORE_WRITING, 0) == 0);
    read_file(in_profile("work", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0);
}

/* A creation that fails leaves no directory behind, unless the directory
 * was there before or the file was written. */
static void a_failed_write_of_a_new_profile(void)
{
    const char *text = DEFINITION("fresh", " {}\n");
    struct profile stored;
    assert(apply("fresh", text, SAVE_FAILS_BEFORE_WRITING, 0) == 1);
    assert(!exists(in_profile("fresh", NULL)));
    assert(mkdir(in_profile("fresh", NULL), 0700) == 0);
    assert(apply("fresh", text, SAVE_FAILS_BEFORE_WRITING, 0) == 1);
    assert(exists(in_profile("fresh", NULL)) &&
           !exists(in_profile("fresh", "config.yaml")));
    assert(rmdir(in_profile("fresh", NULL)) == 0);
    assert(apply("fresh", text, SAVE_FAILS_AFTER_WRITING, 0) == 5);
    assert(profile_read_existing(&stored, "fresh") == 0 && stored.cpus == 4);
    assert(apply("fresh", text, SAVE_WORKS, 0) == 0);
}

/* The first look at the stored settings is taken without a lock. What a
 * change does is decided again once the lock is held, from what the profile
 * holds then. */
static void a_change_is_decided_again_under_the_lock(void)
{
    char after[4096];
    assert(apply("moved", DEFINITION("moved", "\n  cpus: 2\n"), SAVE_WORKS,
                 0) == 0);

    /* The settings became equal meanwhile: nothing is written, and the
     * other writer's file stays as it is, comment included. */
    const char *equal = "# written by another\ncpus: 6\n";
    replacement_after_read = equal;
    assert(apply("moved", DEFINITION("moved", "\n  cpus: 6\n"), SAVE_WORKS,
                 0) == 0);
    assert(saves == 0 && !replacement_after_read);
    read_file(in_profile("moved", "config.yaml"), after, sizeof(after));
    assert(strcmp(after, equal) == 0);

    /* The disk grew meanwhile: the definition would now shrink it, and is
     * refused against the size that the profile has under the lock. */
    const char *grown = "cpus: 6\ndiskGiB: 100\n";
    replacement_after_read = grown;
    assert(apply("moved", DEFINITION("moved", "\n  cpus: 8\n  diskGiB: 80\n"),
                 SAVE_WORKS, 0) == 4);
    assert(strstr(log_last_error(),
                  "disk size cannot shrink (current: 100 GiB)"));
    assert(saves == 0);
    read_file(in_profile("moved", "config.yaml"), after, sizeof(after));
    assert(strcmp(after, grown) == 0);
}

/* The write succeeded and no result can be built for it: the call must not
 * report that config.yaml is unchanged. */
static void a_written_change_without_a_result(void)
{
    struct profile stored;
    assert(apply("silent", DEFINITION("silent", "\n  cpus: 2\n"), SAVE_WORKS,
                 0) == 0);
    assert(apply("silent", DEFINITION("silent", "\n  cpus: 4\n"),
                 SAVE_WORKS_AND_NO_RESULT_CAN_BE_BUILT, 0) == 5);
    assert(strcmp(log_last_error(),
                  "cannot build the result of vm apply") == 0);
    assert(saves == 1);
    assert(profile_read_existing(&stored, "silent") == 0 && stored.cpus == 4);
}

int main(void)
{
    char temporary[] = "/tmp/hamn-apply-faults-XXXXXX";
    assert(mkdtemp(temporary));
    snprintf(home, sizeof(home), "%s", temporary);
    snprintf(definition, sizeof(definition), "%s/definition.yaml", home);
    assert(setenv("HOME", home, 1) == 0);
    pid_t runner = fork();
    assert(runner >= 0);
    if (runner == 0) {
        alarm(60);
        a_failed_write_of_an_existing_profile();
        a_failed_write_of_a_new_profile();
        a_change_is_decided_again_under_the_lock();
        a_written_change_without_a_result();
        _exit(0);
    }
    int status = 0;
    pid_t reaped;
    do {
        reaped = waitpid(runner, &status, 0);
    } while (reaped < 0 && errno == EINTR);
    char command[sizeof(temporary) + 16];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", temporary);
    int removed = system(command) == 0;
    if (reaped != runner || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: failed profile writes (wait status %d)\n",
                status);
        return 1;
    }
    assert(removed);
    puts("PASS: a failed profile write is reported by what it left");
    return 0;
}
