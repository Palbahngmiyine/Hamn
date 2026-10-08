/* hamn_control_apply() makes config.yaml equal to a profile definition file:
 * it creates the profile, replaces differing settings, or does nothing, and
 * refuses everything else without changing a byte. No VM is involved here;
 * tests/host/test_vmrun_identity.c covers a running VM, and
 * tests/host/test_profile_apply_faults.c a write that fails. */
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/control.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/profile.h"
#include "util/fs.h"
#include "util/proc.h"

#define DEFINITION(name, spec) \
    "apiVersion: hamn/v1\nkind: Profile\nmetadata:\n  name: " name "\nspec:" spec

static char home[64];        /* HOME of this test */
static char hamn[96];        /* HOME/.hamn */
static char definition[96];  /* the definition file that each call rewrites */

static void write_file(const char *path, const char *data, size_t length)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    assert(fd >= 0 && write(fd, data, length) == (ssize_t)length &&
           close(fd) == 0);
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

static const char *in_profile(const char *name, const char *file)
{
    static char path[256];
    snprintf(path, sizeof(path), "%s/%s%s%s", hamn, name, file ? "/" : "",
             file ? file : "");
    return path;
}

static int exists(const char *path)
{
    struct stat status;
    return lstat(path, &status) == 0;
}

/* One apply of `text` for `profile`. A result is returned exactly on 0. */
static int apply(const char *profile, const char *text, int dry_run,
                 cJSON **result)
{
    write_file(definition, text, strlen(text));
    char *json = NULL;
    int rc = hamn_control_apply(profile, definition, dry_run, &json);
    assert((rc == 0) == (json != NULL));
    cJSON *value = json ? cJSON_Parse(json) : NULL;
    assert(!json || value);
    hamn_control_free(json);
    if (result)
        *result = value;
    else
        cJSON_Delete(value);
    return rc;
}

/* An apply that must fail with `code`, a reason that contains `reason`, and
 * no result. */
static void refused(const char *profile, const char *text, int dry_run,
                    int code, const char *reason)
{
    int rc = apply(profile, text, dry_run, NULL);
    if (rc != code || !strstr(log_last_error(), reason)) {
        fprintf(stderr, "expected %d \"%s\", got %d \"%s\" for:\n%s\n", code,
                reason, rc, log_last_error(), text);
        abort();
    }
}

/* An apply that must succeed with `action`; its changes, compact. */
static const char *applied(const char *profile, const char *text, int dry_run,
                           const char *action)
{
    static char changes[1024];
    cJSON *result = NULL;
    int rc = apply(profile, text, dry_run, &result);
    if (rc != 0) {
        fprintf(stderr, "apply failed with %d \"%s\" for:\n%s\n", rc,
                log_last_error(), text);
        abort();
    }
    const cJSON *name = cJSON_GetObjectItemCaseSensitive(result, "profile");
    const cJSON *did = cJSON_GetObjectItemCaseSensitive(result, "action");
    const cJSON *dry = cJSON_GetObjectItemCaseSensitive(result, "dryRun");
    assert(cJSON_IsString(name) && strcmp(name->valuestring, profile) == 0);
    if (!cJSON_IsString(did) || strcmp(did->valuestring, action) != 0) {
        fprintf(stderr, "expected action %s, got %s\n", action,
                cJSON_IsString(did) ? did->valuestring : "none at all");
        abort();
    }
    assert(cJSON_IsBool(dry) && cJSON_IsTrue(dry) == !!dry_run);
    assert(cJSON_GetArraySize(result) == 4);
    char *text_of_changes = cJSON_PrintUnformatted(
        cJSON_GetObjectItemCaseSensitive(result, "changes"));
    assert(text_of_changes);
    snprintf(changes, sizeof(changes), "%s", text_of_changes);
    cJSON_free(text_of_changes);
    cJSON_Delete(result);
    return changes;
}

/* A refused definition names its fault, and a refusal of the request, the
 * file or the definition creates nothing at all: not even ~/.hamn. */
static void refused_definitions_create_nothing(void)
{
    static const struct {
        const char *text;
        const char *reason;
    } cases[] = {
        { "", "expected exactly one YAML configuration document" },
        { "cpus: 4\n", "unknown definition key: cpus" },
        { "kind: Profile\nmetadata:\n  name: work\nspec: {}\n",
          "a profile definition requires apiVersion, kind, metadata.name "
          "and spec" },
        { "apiVersion: hamn/v1\nmetadata:\n  name: work\nspec: {}\n",
          "a profile definition requires" },
        { "apiVersion: hamn/v1\nkind: Profile\nspec: {}\n",
          "a profile definition requires" },
        { "apiVersion: hamn/v1\nkind: Profile\nmetadata: {}\nspec: {}\n",
          "a profile definition requires" },
        { "apiVersion: hamn/v1\nkind: Profile\nmetadata:\n  name: work\n",
          "a profile definition requires" },
        { "apiVersion: hamn/v2\nkind: Profile\nmetadata:\n  name: work\n"
          "spec: {}\n", "apiVersion must be hamn/v1" },
        { "apiVersion: hamn/v1\nkind: VirtualMachine\nmetadata:\n"
          "  name: work\nspec: {}\n", "kind must be Profile" },
        { "apiVersion: hamn/v1\napiVersion: hamn/v1\nkind: Profile\n"
          "metadata:\n  name: work\nspec: {}\n",
          "duplicate or invalid definition key" },
        { DEFINITION("work", " {}\n") "status: {}\n",
          "unknown definition key: status" },
        { "apiVersion: hamn/v1\nkind: Profile\nmetadata:\n  name: work\n"
          "  labels: {}\nspec: {}\n", "unknown metadata key: labels" },
        { "apiVersion: hamn/v1\nkind: Profile\nmetadata: work\nspec: {}\n",
          "expected a mapping" },
        { DEFINITION("../work", " {}\n"),
          "metadata.name is not a valid profile name" },
        { DEFINITION("cache", " {}\n"),
          "metadata.name is not a valid profile name" },
        { DEFINITION("\"\\e[2J\"", " {}\n"),
          "metadata.name is not a valid profile name" },
        { DEFINITION("work", "\n"), "expected a mapping" },
        { DEFINITION("work", " []\n"), "expected a mapping" },
        { DEFINITION("work", "\n  network: shared\n"),
          "unknown configuration key: network" },
        { DEFINITION("work", "\n  state: running\n"),
          "unknown configuration key: state" },
        { DEFINITION("work", "\n  cpus: 0\n"), "expected a positive integer" },
        { DEFINITION("work", "\n  cpus: &cpus 4\n"),
          "YAML anchors and tags are not supported" },
        { DEFINITION("work", "\n  homeReadOnly: true\n  mountHome: false\n"),
          "homeReadOnly requires mountHome" },
        { DEFINITION("work", " {}\n") "---\n" DEFINITION("work", " {}\n"),
          "expected exactly one YAML configuration document" },
        /* A value that config.yaml could not hold: the file written from it
         * would not be read again. */
        { DEFINITION("work", "\n  provision:\n    - command: \"echo \\x7F\"\n"),
          "a setting holds a character that config.yaml cannot store" },
        /* So is a control character, which is never written: its refusal is
         * not the one for settings that are too large. */
        { DEFINITION("work", "\n  provision:\n    - command: \"echo \\a\"\n"),
          "a setting holds a character that config.yaml cannot store" },
        { DEFINITION("other", " {}\n"),
          "the definition names profile other but --profile is work" },
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        refused("work", cases[index].text, 0, 2, cases[index].reason);
        refused("work", cases[index].text, 1, 2, cases[index].reason);
    }
    /* The request itself. */
    char *json = (char *)definition;
    assert(hamn_control_apply("../work", definition, 0, &json) == 2 && !json);
    assert(hamn_control_apply(NULL, definition, 0, &json) == 2);
    assert(hamn_control_apply("work", NULL, 0, &json) == 2);
    assert(hamn_control_apply("work", "", 0, &json) == 2);
    assert(hamn_control_apply("work", definition, 0, NULL) == 2);
    /* The file: missing, a directory, a FIFO (which must not block), and
     * one byte over the limit. */
    char path[128];
    snprintf(path, sizeof(path), "%s/missing.yaml", home);
    assert(hamn_control_apply("work", path, 0, &json) == 2);
    assert(strstr(log_last_error(), "cannot read the profile definition"));
    assert(hamn_control_apply("work", home, 0, &json) == 2);
    assert(strstr(log_last_error(), "is not a regular file"));
    snprintf(path, sizeof(path), "%s/fifo.yaml", home);
    assert(mkfifo(path, 0600) == 0);
    assert(hamn_control_apply("work", path, 0, &json) == 2);
    assert(strstr(log_last_error(), "is not a regular file"));
    assert(unlink(path) == 0);
    /* Settings that fit the definition file and not config.yaml, which
     * writes each backslash of a path as two bytes: refused for their size,
     * and not as if a character were at fault. */
    static char bulky[40 * 1024];
    char backslashes[1011];
    memset(backslashes, '\\', sizeof(backslashes) - 1);
    backslashes[sizeof(backslashes) - 1] = '\0';
    size_t used = (size_t)snprintf(bulky, sizeof(bulky), "%s",
                                   DEFINITION("work", "\n  mounts:\n"));
    for (int index = 0; index < 16; index++)
        used += (size_t)snprintf(bulky + used, sizeof(bulky) - used,
                                 "    - location: '/%s'\n"
                                 "      mountPoint: '/%s%02d'\n",
                                 backslashes, backslashes, index);
    assert(used < sizeof(bulky));
    for (int dry_run = 0; dry_run < 2; dry_run++)
        refused("work", bulky, dry_run, 2,
                "its settings do not fit in config.yaml");
    static char large[64 * 1024 + 2];
    const char *valid = DEFINITION("work", " {}\n");
    memset(large, '#', sizeof(large) - 1);
    memcpy(large, valid, strlen(valid));
    large[sizeof(large) - 1] = '\0';
    refused("work", large, 0, 2, "is larger than 65536 bytes");
    /* At the limit the definition is read; a dry run still creates nothing. */
    large[64 * 1024] = '\0';
    assert(strcmp(applied("work", large, 1, "create"), "[]") == 0);
    assert(!exists(hamn));
}

/* Creation, a repeated apply, and a change: what each writes and reports. */
static void apply_creates_configures_or_does_nothing(void)
{
    const char *first = DEFINITION("work",
        "\n  cpus: 6\n  memoryMiB: 6000\n  diskGiB: 80\n  rosetta: true\n"
        "  docker:\n    daemonJson: '{\"debug\":true}'\n"
        "  mounts:\n    - location: /Volumes/data\n"
        "      mountPoint: /data\n"
        "  provision:\n    - command: echo ready\n");
    assert(strcmp(applied("work", first, 1, "create"), "[]") == 0);
    assert(!exists(hamn));
    assert(strcmp(applied("work", first, 0, "create"), "[]") == 0);
    struct stat status;
    assert(stat(in_profile("work", NULL), &status) == 0 &&
           (status.st_mode & 07777) == 0700);
    assert(stat(in_profile("work", "config.yaml"), &status) == 0 &&
           (status.st_mode & 07777) == 0600);
    struct profile stored;
    assert(profile_read_existing(&stored, "work") == 0);
    assert(stored.cpus == 6 && stored.mem_mib == 6000 &&
           stored.disk_gib == 80 && stored.rosetta == 1 &&
           stored.mount_home == 1 && stored.mount_count == 1 &&
           stored.hook_count == 1 && stored.hooks[0].timeout_seconds == 60);
    assert(strcmp(stored.docker_daemon_json, "{\"debug\":true}") == 0);
    assert(strcmp(stored.mounts[0].mount_point, "/data") == 0);

    /* The same definition again: no write. The file keeps its bytes, its
     * inode and its modification time, which decides whether the next start
     * rebuilds the seed. */
    char before[8192], after[8192];
    size_t length = read_file(in_profile("work", "config.yaml"), before,
                              sizeof(before));
    const struct timeval past[2] = { { 1000000000, 0 }, { 1000000000, 0 } };
    assert(utimes(in_profile("work", "config.yaml"), past) == 0);
    assert(stat(in_profile("work", "config.yaml"), &status) == 0);
    ino_t inode = status.st_ino;
    assert(strcmp(applied("work", first, 0, "none"), "[]") == 0);
    assert(strcmp(applied("work", first, 1, "none"), "[]") == 0);
    assert(stat(in_profile("work", "config.yaml"), &status) == 0);
    assert(status.st_ino == inode && status.st_mtime == 1000000000);
    assert(read_file(in_profile("work", "config.yaml"), after,
                     sizeof(after)) == length && strcmp(before, after) == 0);

    /* A changed definition is the whole configuration: the keys it leaves
     * out return to their defaults, and every change is named. Numbers and
     * booleans carry their values; the other settings only their key. */
    const char *second = DEFINITION("work",
        "\n  cpus: 8\n  memoryMiB: 6000\n  diskGiB: 80\n  sshAgent: true\n"
        "  mounts:\n    - location: /Volumes/data\n"
        "      mountPoint: /data\n      writable: true\n");
    const char *expected =
        "[{\"key\":\"cpus\",\"from\":6,\"to\":8},"
        "{\"key\":\"docker.daemonJson\"},"
        "{\"key\":\"rosetta\",\"from\":true,\"to\":false},"
        "{\"key\":\"sshAgent\",\"from\":false,\"to\":true},"
        "{\"key\":\"mounts\"},{\"key\":\"provision\"}]";
    assert(strcmp(applied("work", second, 1, "configure"), expected) == 0);
    assert(read_file(in_profile("work", "config.yaml"), after,
                     sizeof(after)) == length && strcmp(before, after) == 0);
    assert(stat(in_profile("work", "config.yaml"), &status) == 0 &&
           status.st_mtime == 1000000000);
    assert(strcmp(applied("work", second, 0, "configure"), expected) == 0);
    assert(profile_read_existing(&stored, "work") == 0);
    assert(stored.cpus == 8 && stored.rosetta == 0 && stored.ssh_agent == 1 &&
           stored.docker_daemon_json[0] == '\0' && stored.hook_count == 0 &&
           stored.mounts[0].writable == 1 && stored.disk_gib == 80);
    assert(stat(in_profile("work", "config.yaml"), &status) == 0 &&
           (status.st_mode & 07777) == 0600);
    assert(strcmp(applied("work", second, 0, "none"), "[]") == 0);

    /* Settings compare by meaning: a hand-written file with the same
     * settings is left alone, comments included. */
    const char *by_hand = "# kept by hand\nsshAgent: true\ncpus: 8\n"
        "memoryMiB: 6000\ndiskGiB: 80\nmounts:\n  - mountPoint: /data\n"
        "    location: /Volumes/data\n    writable: true\n";
    write_file(in_profile("work", "config.yaml"), by_hand, strlen(by_hand));
    assert(strcmp(applied("work", second, 0, "none"), "[]") == 0);
    read_file(in_profile("work", "config.yaml"), after, sizeof(after));
    assert(strcmp(after, by_hand) == 0);
}

/* States of a profile that forbid the change. Each leaves every byte. */
static void conflicting_states_are_refused_unchanged(void)
{
    char before[8192], after[8192];
    /* The disk never shrinks, and a definition that leaves diskGiB out asks
     * for the default 60. */
    assert(strcmp(applied("grown", DEFINITION("grown", "\n  diskGiB: 80\n"),
                          0, "create"), "[]") == 0);
    read_file(in_profile("grown", "config.yaml"), before, sizeof(before));
    for (int dry_run = 0; dry_run < 2; dry_run++) {
        refused("grown", DEFINITION("grown", "\n  cpus: 2\n"), dry_run, 4,
                "disk size cannot shrink (current: 80 GiB); set spec.diskGiB "
                "to 80 or more");
        refused("grown", DEFINITION("grown", "\n  diskGiB: 79\n"), dry_run, 4,
                "disk size cannot shrink");
    }
    read_file(in_profile("grown", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0);
    assert(strcmp(applied("grown", DEFINITION("grown", "\n  diskGiB: 81\n"),
                          0, "configure"),
                  "[{\"key\":\"diskGiB\",\"from\":80,\"to\":81}]") == 0);

    /* A deleted profile stays deleted, even for equal settings: only a start
     * brings its disk back. */
    const char *gone = DEFINITION("gone", "\n  cpus: 2\n");
    assert(strcmp(applied("gone", gone, 0, "create"), "[]") == 0);
    write_file(in_profile("gone", "deleted"), "soft-deleted\n", 13);
    read_file(in_profile("gone", "config.yaml"), before, sizeof(before));
    for (int dry_run = 0; dry_run < 2; dry_run++) {
        refused("gone", gone, dry_run, 4, "profile gone is deleted");
        refused("gone", DEFINITION("gone", "\n  cpus: 4\n"), dry_run, 4,
                "vm start --profile gone restores it");
    }
    read_file(in_profile("gone", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0 && exists(in_profile("gone", "deleted")));
    /* So does one that was deleted without ever having a configuration. */
    assert(mkdir(in_profile("tombstone", NULL), 0700) == 0);
    write_file(in_profile("tombstone", "deleted"), "soft-deleted\n", 13);
    refused("tombstone", DEFINITION("tombstone", " {}\n"), 0, 4,
            "profile tombstone is deleted");
    assert(!exists(in_profile("tombstone", "config.yaml")));

    /* A directory without config.yaml becomes the profile when it holds
     * nothing but what an interrupted or refused creation leaves: no entry,
     * the temporary file of a configuration write (which is removed), or the
     * operation record of a first start. */
    assert(mkdir(in_profile("interrupted", NULL), 0700) == 0);
    assert(strcmp(applied("interrupted", DEFINITION("interrupted", " {}\n"),
                          0, "create"), "[]") == 0);
    assert(exists(in_profile("interrupted", "config.yaml")));
    assert(mkdir(in_profile("killed", NULL), 0700) == 0);
    write_file(in_profile("killed", "config.yaml.tmp.1.deadbeef"), "cpus", 4);
    write_file(in_profile("killed", "operation.json"), "{}", 2);
    write_file(in_profile("killed", "operation.json.tmp.1.deadbeef"), "{", 1);
    write_file(in_profile("killed", "state.json"), "{}", 2);
    write_file(in_profile("killed", "state.json.tmp.1.deadbeef"), "{", 1);
    write_file(in_profile("killed", "port-forward-operations.lock"), "", 0);
    write_file(in_profile("killed", "port-forwards.lock"), "", 0);
    assert(strcmp(applied("killed", DEFINITION("killed", " {}\n"), 1,
                          "create"), "[]") == 0);
    assert(exists(in_profile("killed", "config.yaml.tmp.1.deadbeef")));
    assert(strcmp(applied("killed", DEFINITION("killed", " {}\n"), 0,
                          "create"), "[]") == 0);
    assert(exists(in_profile("killed", "config.yaml")) &&
           exists(in_profile("killed", "operation.json")) &&
           !exists(in_profile("killed", "config.yaml.tmp.1.deadbeef")));
    /* A directory that holds anything else is not adopted: a disk, or the
     * archives of `vm diagnostics`, whose default directory has the name of
     * a possible profile. */
    assert(mkdir(in_profile("diagnostics", NULL), 0700) == 0);
    write_file(in_profile("diagnostics", "config.yaml.tmp.1.deadbeef"), "c", 1);
    write_file(in_profile("diagnostics", "disk.img"), "data", 4);
    for (int dry_run = 0; dry_run < 2; dry_run++)
        refused("diagnostics", DEFINITION("diagnostics", " {}\n"), dry_run, 4,
                "holds files but no config.yaml");
    assert(!exists(in_profile("diagnostics", "config.yaml")) &&
           exists(in_profile("diagnostics", "config.yaml.tmp.1.deadbeef")));

    /* A stored configuration that cannot be read is not replaced: the
     * reason is the parser's. */
    assert(mkdir(in_profile("broken", NULL), 0700) == 0);
    write_file(in_profile("broken", "config.yaml"), "network: shared\n", 16);
    for (int dry_run = 0; dry_run < 2; dry_run++)
        refused("broken", DEFINITION("broken", " {}\n"), dry_run, 1,
                "cannot read the configuration of profile broken: unknown "
                "configuration key: network");
    read_file(in_profile("broken", "config.yaml"), after, sizeof(after));
    assert(strcmp(after, "network: shared\n") == 0);

    /* A profile directory that is a link is refused; nothing is written
     * through it. */
    char target[128];
    snprintf(target, sizeof(target), "%s/elsewhere", home);
    assert(mkdir(target, 0700) == 0);
    assert(symlink(target, in_profile("linked", NULL)) == 0);
    refused("linked", DEFINITION("linked", " {}\n"), 0, 1,
            "cannot read the configuration of profile linked");
    assert(rmdir(target) == 0 && unlink(in_profile("linked", NULL)) == 0);
}

static void wait_for(pid_t child, int *status)
{
    pid_t reaped;
    do {
        reaped = waitpid(child, status, 0);
    } while (reaped < 0 && errno == EINTR);
    assert(reaped == child);
}

/* The child's exit status is the return value of its apply. */
static pid_t apply_in_child(const char *profile, const char *path, int cancel,
                            int ready_fd)
{
    pid_t child = fork();
    assert(child >= 0);
    if (child != 0)
        return child;
    alarm(30);
    if (proc_cancel_install() != 0)
        _exit(99);
    if (cancel)
        kill(getpid(), SIGTERM);
    if (ready_fd >= 0 && write(ready_fd, "r", 1) != 1)
        _exit(98);
    char *json = NULL;
    _exit(hamn_control_apply(profile, path, 0, &json));
}

/* Holds the lifecycle lock of `profile` until release_lock() is called with
 * the returned descriptor. A byte releases it, not the end of the pipe: a
 * child that is forked later inherits the writing end. */
static pid_t hold_lifecycle_lock(const char *profile, int *release_fd)
{
    int held[2], release[2];
    assert(pipe(held) == 0 && pipe(release) == 0);
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        alarm(30);
        close(held[0]);
        close(release[1]);
        struct vm_lifecycle_lock lock;
        char byte = 'h';
        if (vm_lifecycle_lock_acquire(profile, &lock) != 0 ||
            write(held[1], &byte, 1) != 1)
            _exit(97);
        ssize_t released;
        do {
            released = read(release[0], &byte, 1);
        } while (released < 0 && errno == EINTR);
        if (released != 1)
            _exit(95);
        vm_lifecycle_lock_release(&lock);
        _exit(0);
    }
    close(held[1]);
    close(release[0]);
    char byte = 0;
    assert(read(held[0], &byte, 1) == 1 && byte == 'h');
    close(held[0]);
    *release_fd = release[1];
    return child;
}

static void release_lock(int release_fd)
{
    assert(write(release_fd, "x", 1) == 1 && close(release_fd) == 0);
}

/* Whether the child is still running after it had `milliseconds` to end. */
static int still_running_after(pid_t child, int milliseconds)
{
    const struct timespec pause = { 0, 10 * 1000 * 1000 };
    for (int waited = 0; waited < milliseconds; waited += 10) {
        int status;
        if (waitpid(child, &status, WNOHANG) != 0)
            return 0;
        nanosleep(&pause, NULL);
    }
    return 1;
}

/* A change waits for the lifecycle lock of its profile and decides again
 * once it holds it. A call that was cancelled meanwhile writes nothing. */
static void a_change_waits_for_the_lifecycle_lock(void)
{
    char change[128], before[8192], after[8192];
    int status, release_fd, ready[2];
    assert(strcmp(applied("queued", DEFINITION("queued", "\n  cpus: 2\n"), 0,
                          "create"), "[]") == 0);
    snprintf(change, sizeof(change), "%s/change.yaml", home);
    const char *text = DEFINITION("queued", "\n  cpus: 3\n");
    write_file(change, text, strlen(text));
    read_file(in_profile("queued", "config.yaml"), before, sizeof(before));

    /* Cancelled before the call: the lock is free, and nothing is written. */
    pid_t child = apply_in_child("queued", change, 1, -1);
    wait_for(child, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 130);
    read_file(in_profile("queued", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0);

    /* Cancelled while another operation holds the profile: the call keeps
     * waiting (the wait cannot be interrupted) and gives up, without a
     * write, once it has the lock. */
    pid_t holder = hold_lifecycle_lock("queued", &release_fd);
    assert(pipe(ready) == 0);
    child = apply_in_child("queued", change, 0, ready[1]);
    close(ready[1]);
    char byte = 0;
    assert(read(ready[0], &byte, 1) == 1 && byte == 'r');
    close(ready[0]);
    assert(still_running_after(child, 200));
    assert(kill(child, SIGTERM) == 0);
    assert(still_running_after(child, 200));
    release_lock(release_fd);
    wait_for(holder, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    wait_for(child, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 130);
    read_file(in_profile("queued", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0);

    /* The settings became equal while the call waited: it finds that under
     * the lock and writes nothing. */
    holder = hold_lifecycle_lock("queued", &release_fd);
    child = apply_in_child("queued", change, 0, -1);
    assert(still_running_after(child, 200));
    /* Replaced in one step: the waiting call may read at any moment. */
    assert(fs_write_file_atomic(in_profile("queued", "config.yaml"),
                                "cpus: 3\n", 8, 0600) == 0);
    release_lock(release_fd);
    wait_for(holder, &status);
    wait_for(child, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    read_file(in_profile("queued", "config.yaml"), after, sizeof(after));
    assert(strcmp(after, "cpus: 3\n") == 0);

    /* Equal settings take no lock at all: the call returns while another
     * operation still holds the profile. */
    holder = hold_lifecycle_lock("queued", &release_fd);
    child = apply_in_child("queued", change, 0, -1);
    wait_for(child, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    release_lock(release_fd);
    wait_for(holder, &status);

    /* The mutation lock is not waited for: while another mutation of the
     * profile holds it, a change fails and writes nothing. The lifecycle
     * lock taken on the way is released, or the next call would not
     * return. */
    char lock_path[160];
    snprintf(lock_path, sizeof(lock_path), "%s/.queued-mutation.lock", hamn);
    int lock = open(lock_path, O_RDWR | O_CREAT, 0600);
    assert(lock >= 0 && flock(lock, LOCK_EX | LOCK_NB) == 0);
    read_file(in_profile("queued", "config.yaml"), before, sizeof(before));
    refused("queued", DEFINITION("queued", "\n  cpus: 5\n"), 0, 1,
            "another queued profile mutation is running");
    read_file(in_profile("queued", "config.yaml"), after, sizeof(after));
    assert(strcmp(before, after) == 0);
    assert(close(lock) == 0);
    assert(strcmp(applied("queued", DEFINITION("queued", "\n  cpus: 5\n"), 0,
                          "configure"),
                  "[{\"key\":\"cpus\",\"from\":3,\"to\":5}]") == 0);
}

/* ~/.hamn that another user could write is not trusted with a profile:
 * nothing is read from it or created in it. */
static void an_untrusted_home_is_refused(void)
{
    assert(chmod(hamn, 0777) == 0);
    for (int dry_run = 0; dry_run < 2; dry_run++)
        refused("untrusted", DEFINITION("untrusted", " {}\n"), dry_run, 1,
                "cannot locate profile untrusted");
    assert(!exists(in_profile("untrusted", NULL)));
    assert(chmod(hamn, 0700) == 0);
}

/* Two definitions applied at the same moment: both succeed, one after the
 * other, and the profile holds one of them completely. */
static void concurrent_applies_leave_one_definition(void)
{
    char paths[2][128];
    const char *texts[2] = {
        DEFINITION("raced", "\n  cpus: 5\n  rosetta: true\n"),
        DEFINITION("raced", "\n  cpus: 7\n  sshAgent: true\n"),
    };
    assert(strcmp(applied("raced", DEFINITION("raced", " {}\n"), 0, "create"),
                  "[]") == 0);
    int gate[2];
    assert(pipe(gate) == 0);
    pid_t children[2];
    for (int index = 0; index < 2; index++) {
        snprintf(paths[index], sizeof(paths[index]), "%s/raced-%d.yaml", home,
                 index);
        write_file(paths[index], texts[index], strlen(texts[index]));
        children[index] = fork();
        assert(children[index] >= 0);
        if (children[index] == 0) {
            char byte;
            alarm(30);
            close(gate[1]);
            /* End of file: the parent released both children at once. */
            if (read(gate[0], &byte, 1) != 0)
                _exit(96);
            char *json = NULL;
            _exit(hamn_control_apply("raced", paths[index], 0, &json));
        }
    }
    close(gate[0]);
    close(gate[1]);
    for (int index = 0; index < 2; index++) {
        int status;
        wait_for(children[index], &status);
        /* The second call waited for the lifecycle lock of the first. */
        assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    }
    struct profile stored;
    assert(profile_read_existing(&stored, "raced") == 0);
    assert((stored.cpus == 5 && stored.rosetta && !stored.ssh_agent) ||
           (stored.cpus == 7 && !stored.rosetta && stored.ssh_agent));
}

int main(void)
{
    char temporary[] = "/tmp/hamn-profile-apply-XXXXXX";
    assert(mkdtemp(temporary));
    snprintf(home, sizeof(home), "%s", temporary);
    snprintf(hamn, sizeof(hamn), "%s/.hamn", home);
    snprintf(definition, sizeof(definition), "%s/definition.yaml", home);
    assert(setenv("HOME", home, 1) == 0);
    pid_t runner = fork();
    assert(runner >= 0);
    if (runner == 0) {
        /* The cases cannot outlive this limit; neither can their children,
         * which set their own. They share one process group, so that none
         * is left to act after a failed case. */
        if (setpgid(0, 0) != 0)
            _exit(94);
        alarm(60);
        refused_definitions_create_nothing();
        apply_creates_configures_or_does_nothing();
        conflicting_states_are_refused_unchanged();
        a_change_waits_for_the_lifecycle_lock();
        concurrent_applies_leave_one_definition();
        an_untrusted_home_is_refused();
        _exit(0);
    }
    int status = 0;
    wait_for(runner, &status);
    /* Whatever a failed case left running must not write into the directory
     * after it is removed. ESRCH: nothing was left. */
    if (kill(-runner, SIGKILL) != 0)
        assert(errno == ESRCH);
    char command[sizeof(temporary) + 16];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", temporary);
    int removed = system(command) == 0;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: profile definition cases (wait status %d)\n",
                status);
        return 1;
    }
    assert(removed);
    puts("PASS: a profile definition is applied exactly, or refused without "
         "a change");
    return 0;
}
