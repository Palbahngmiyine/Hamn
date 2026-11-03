/* hamn_control_purge() moves a profile directory out of the profile
 * namespace before it removes it, so an interruption at any point leaves no
 * profile behind, and the next list or purge removes what is left. Faults
 * come from purge.c's HAMN_TEST hook, from a directory the test makes
 * unremovable, and from a stop that leaves a VM process behind. No VM is
 * involved here; the vm-purge suite covers the headless contract, its
 * refusals and its error codes. */
#include <assert.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/control.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/profile.h"
#include "core/purge.h"

/* cmd_delete.c is built with vm_stop renamed to fault_vm_stop (Makefile). */
#undef vm_stop
int vm_stop(const struct profile *p, int *was_running);

static char root[64];   /* the directory of this test */
static char home[128];  /* HOME of the current case */
static char hamn[160];  /* HOME/.hamn */

static void begin(const char *name)
{
    snprintf(home, sizeof(home), "%s/%s", root, name);
    snprintf(hamn, sizeof(hamn), "%s/.hamn", home);
    assert(mkdir(home, 0700) == 0 && setenv("HOME", home, 1) == 0);
}

/* The real stop, then (while spawn_guard_planned) a vmrun that begins
 * spawning after it: the stop reported success, yet vm_process_probe() finds
 * the profile's spawn guard locked and cannot call the VM stopped. */
static int spawn_guard_planned;
static int spawn_guard = -1;  /* the descriptor that holds that lock */
static int stops_reported_ok;

static void spawn_guard_release(void)
{
    if (spawn_guard >= 0)
        assert(close(spawn_guard) == 0);
    spawn_guard = -1;
}

int fault_vm_stop(const struct profile *p, int *was_running)
{
    int result = vm_stop(p, was_running);
    if (result != VM_STOP_OK)
        return result;
    stops_reported_ok++;
    if (spawn_guard_planned) {
        char path[PROFILE_PATH_CAP];
        assert(profile_path(p, "vmrun.spawning", path, sizeof(path)));
        spawn_guard = open(path, O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
        assert(spawn_guard >= 0 && flock(spawn_guard, LOCK_EX | LOCK_NB) == 0);
    }
    return result;
}

static const char *at(const char *directory, const char *name)
{
    static char path[512];
    snprintf(path, sizeof(path), "%s/%s", directory, name);
    return path;
}

static int exists(const char *path)
{
    struct stat status;
    return lstat(path, &status) == 0;
}

static void write_file(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    size_t length = strlen(text);
    assert(fd >= 0 && write(fd, text, length) == (ssize_t)length &&
           close(fd) == 0);
}

/* A created profile with a disk, a log, and a link to a directory outside
 * ~/.hamn that holds `keep`. */
static void create(const char *profile)
{
    assert(hamn_control_configure(profile, 0, 0, 0, 1, -1) == 0);
    char directory[256], path[512];
    snprintf(directory, sizeof(directory), "%s/%s", hamn, profile);
    write_file(at(directory, "disk.img"), "disk\n");
    assert(mkdir(at(directory, "logs"), 0700) == 0);
    write_file(at(directory, "logs/serial.log"), "boot\n");
    snprintf(path, sizeof(path), "%s/outside-%s", home, profile);
    assert(mkdir(path, 0700) == 0);
    write_file(at(path, "keep"), "keep\n");
    assert(symlink(path, at(directory, "shared")) == 0);
}

static int outside_kept(const char *profile)
{
    char path[256];
    snprintf(path, sizeof(path), "%s/outside-%s/keep", home, profile);
    return exists(path);
}

/* The number of ~/.hamn entries whose name starts with .purge-; the last
 * one's name goes to name. */
static int leftovers(char name[256])
{
    DIR *directory = opendir(hamn);
    assert(directory);
    int count = 0;
    struct dirent *entry;
    while ((entry = readdir(directory))) {
        if (strncmp(entry->d_name, ".purge-", 7) == 0) {
            count++;
            if (name)
                snprintf(name, 256, "%s", entry->d_name);
        }
    }
    assert(closedir(directory) == 0);
    return count;
}

static int purge(const char *profile, int *absent)
{
    *absent = -1;
    int rc = hamn_control_purge(profile, absent);
    assert(*absent == 0 || *absent == 1);
    return rc;
}

/* vm list: whether it names profile. The list itself must succeed. */
static int listed(const char *profile)
{
    char *json = NULL;
    assert(hamn_control_query(NULL, &json) == 0 && json);
    cJSON *rows = cJSON_Parse(json);
    hamn_control_free(json);
    assert(cJSON_IsArray(rows));
    int found = 0;
    const cJSON *row;
    cJSON_ArrayForEach(row, rows) {
        const cJSON *name = cJSON_GetObjectItemCaseSensitive(row, "name");
        found |= cJSON_IsString(name) && strcmp(name->valuestring, profile) == 0;
    }
    cJSON_Delete(rows);
    return found;
}

static int recorded(const char *text)
{
    return strstr(log_last_error(), text) != NULL;
}

static void a_purge_removes_the_profile_but_not_what_it_links_to(void)
{
    begin("removes");
    create("work");
    create("other");
    int absent;
    assert(purge("work", &absent) == 0 && absent == 0);
    assert(!exists(at(hamn, "work")) && leftovers(NULL) == 0);
    assert(outside_kept("work") && exists(at(hamn, "other/disk.img")));
    assert(!listed("work") && listed("other"));
    /* The lock files stay, empty (core/control.h says why). */
    struct stat lock;
    assert(lstat(at(hamn, ".locks/work.lock"), &lock) == 0 &&
           lock.st_size == 0);
    assert(lstat(at(hamn, ".work-mutation.lock"), &lock) == 0 &&
           lock.st_size == 0);
    assert(purge("work", &absent) == 0 && absent == 1);
}

static void an_interrupted_purge_is_hidden_and_the_next_list_removes_it(void)
{
    begin("interrupted-list");
    create("work");
    purge_test_interrupt_after_rename();
    int absent;
    assert(purge("work", &absent) == 1 && absent == 0);
    assert(recorded("interrupted after moving profile work"));
    char name[256];
    assert(!exists(at(hamn, "work")) && leftovers(name) == 1);
    assert(strlen(name) == strlen(".purge-work-") + 16);
    char left[512];
    snprintf(left, sizeof(left), "%s/%s", hamn, name);
    assert(exists(at(left, "disk.img")) && exists(at(left, "logs/serial.log")));

    char *json = NULL;
    assert(hamn_control_query("work", &json) == -1 && !json);
    assert(strcmp(log_last_error(), "profile work does not exist") == 0);
    assert(!listed("work"));
    assert(leftovers(NULL) == 0 && outside_kept("work"));
}

static void an_interrupted_purge_is_removed_by_the_next_purge(void)
{
    begin("interrupted-purge");
    create("work");
    create("other");
    purge_test_interrupt_after_rename();
    int absent;
    assert(purge("work", &absent) == 1 && leftovers(NULL) == 1);
    assert(purge("work", &absent) == 0 && absent == 1);
    assert(leftovers(NULL) == 0 && exists(at(hamn, "other/disk.img")));
}

/* A directory without write permission keeps its entries from removal: the
 * purge fails after the rename, and so does the first sweep after it. */
static void a_tree_that_cannot_be_removed_stays_hidden_until_it_can(void)
{
    begin("unremovable");
    create("work");
    char held[512];
    snprintf(held, sizeof(held), "%s/work/held", hamn);
    assert(mkdir(held, 0700) == 0);
    write_file(at(held, "file"), "x\n");
    assert(chmod(held, 0500) == 0);
    int absent;
    assert(purge("work", &absent) == 1 && absent == 0);
    assert(recorded("cannot remove") && recorded("the deleted profile work") &&
           recorded("Permission denied"));
    char name[256];
    assert(!exists(at(hamn, "work")) && leftovers(name) == 1);

    assert(!listed("work") && leftovers(NULL) == 1);
    snprintf(held, sizeof(held), "%s/%s/held", hamn, name);
    assert(chmod(held, 0700) == 0);
    assert(!listed("work") && leftovers(NULL) == 0);
}

static void the_sweep_removes_only_purged_trees(void)
{
    begin("shapes");
    create("work");
    const char *kept[] = {
        ".purge-work-0123456789ABCDEF", /* upper-case digits */
        ".purge-work-0123456789abcde",  /* 15 digits */
        ".purge-work0123456789abcdef",  /* no separator */
        ".purge--0123456789abcdef",     /* no profile name */
        ".purge-cache-0123456789abcdef", /* cache is no profile name */
        ".purge-work-0123456789abcdef-x",
        NULL,
    };
    for (int i = 0; kept[i]; i++)
        assert(mkdir(at(hamn, kept[i]), 0700) == 0);
    write_file(at(hamn, ".purge-file-0123456789abcdef"), "file\n");
    char outside[256];
    snprintf(outside, sizeof(outside), "%s/outside-work", home);
    assert(symlink(outside, at(hamn, ".purge-link-0123456789abcdef")) == 0);
    char swept[512];
    snprintf(swept, sizeof(swept), "%s/.purge-old-name_1-0123456789abcdef",
             hamn);
    assert(mkdir(swept, 0700) == 0);
    write_file(at(swept, "disk.img"), "disk\n");

    assert(listed("work"));
    assert(!exists(swept));
    for (int i = 0; kept[i]; i++)
        assert(exists(at(hamn, kept[i])));
    assert(exists(at(hamn, ".purge-file-0123456789abcdef")));
    assert(exists(at(hamn, ".purge-link-0123456789abcdef")) &&
           outside_kept("work"));
}

/* The purge that renamed a tree holds the lifecycle lock of its name until
 * it has removed it; a sweep leaves such a tree to that purge. */
static void a_tree_whose_purge_still_runs_is_left_to_it(void)
{
    begin("running");
    create("work");
    char left[512];
    snprintf(left, sizeof(left), "%s/.purge-old-0123456789abcdef", hamn);
    assert(mkdir(left, 0700) == 0);
    struct vm_lifecycle_lock lock;
    assert(vm_lifecycle_lock_acquire("old", &lock) == 0);
    assert(listed("work") && exists(left));
    vm_lifecycle_lock_release(&lock);
    assert(listed("work") && !exists(left));
}

/* Every path under directory, depth first, into text. */
static void tree(const char *directory, char *text, size_t capacity)
{
    DIR *listing = opendir(directory);
    if (!listing) {
        assert(errno == ENOENT);
        return;
    }
    struct dirent *entry;
    while ((entry = readdir(listing))) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0)
            continue;
        char path[512];
        snprintf(path, sizeof(path), "%s/%s", directory, entry->d_name);
        size_t length = strlen(text);
        assert(length + strlen(path) + 2 < capacity);
        snprintf(text + length, capacity - length, "%s\n", path);
        if (entry->d_type == DT_DIR)
            tree(path, text, capacity);
    }
    assert(closedir(listing) == 0);
}

/* A stop that reported success is not enough: a VM process the probe still
 * sees keeps the profile, and it is deleted once the process is gone. */
static void a_stop_that_leaves_a_vm_process_behind_keeps_the_profile(void)
{
    begin("vm-behind");
    create("work");
    spawn_guard_planned = 1;
    stops_reported_ok = 0;
    int absent;
    assert(purge("work", &absent) == HAMN_CONTROL_STOP_UNCERTAIN && absent == 0);
    assert(stops_reported_ok == 1 && spawn_guard >= 0);
    assert(recorded("refusing to delete while VM or host port-forward "
                    "ownership is uncertain"));
    assert(exists(at(hamn, "work/disk.img")) &&
           exists(at(hamn, "work/logs/serial.log")) &&
           exists(at(hamn, "work/config.yaml")) && outside_kept("work"));
    assert(leftovers(NULL) == 0 && listed("work"));

    spawn_guard_planned = 0;
    spawn_guard_release();
    assert(unlink(at(hamn, "work/vmrun.spawning")) == 0);
    assert(purge("work", &absent) == 0 && absent == 0);
    assert(!exists(at(hamn, "work")) && leftovers(NULL) == 0);
}

static void an_absent_profile_is_purged_without_a_trace(void)
{
    begin("absent");
    int absent;
    assert(purge("ghost", &absent) == 0 && absent == 1);
    assert(!exists(hamn));

    create("work");
    static char before[16384], after[16384];
    before[0] = after[0] = '\0';
    tree(home, before, sizeof(before));
    assert(purge("ghost", &absent) == 0 && absent == 1);
    tree(home, after, sizeof(after));
    assert(strcmp(before, after) == 0);
}

static void invalid_arguments_change_nothing(void)
{
    begin("invalid");
    int absent = -1;
    assert(hamn_control_purge("work", NULL) == 2);
    for (const char *const *name = (const char *const[]){ "", "..", "cache",
             "a/b", ".purge-work-0123456789abcdef", NULL }; *name; name++) {
        assert(hamn_control_purge(*name, &absent) == 2 && absent == 0);
    }
    assert(!exists(hamn));
}

int main(void)
{
    char temporary[] = "/tmp/hamn-purge-XXXXXX";
    assert(mkdtemp(temporary));
    snprintf(root, sizeof(root), "%s", temporary);
    pid_t runner = fork();
    assert(runner >= 0);
    if (runner == 0) {
        alarm(60);
        a_purge_removes_the_profile_but_not_what_it_links_to();
        an_interrupted_purge_is_hidden_and_the_next_list_removes_it();
        an_interrupted_purge_is_removed_by_the_next_purge();
        a_tree_that_cannot_be_removed_stays_hidden_until_it_can();
        the_sweep_removes_only_purged_trees();
        a_tree_whose_purge_still_runs_is_left_to_it();
        a_stop_that_leaves_a_vm_process_behind_keeps_the_profile();
        an_absent_profile_is_purged_without_a_trace();
        invalid_arguments_change_nothing();
        _exit(0);
    }
    int status = 0;
    pid_t reaped;
    do {
        reaped = waitpid(runner, &status, 0);
    } while (reaped < 0 && errno == EINTR);
    assert(reaped == runner);
    /* A failed case can leave a directory without write permission. */
    char command[2 * sizeof(temporary) + 48];
    snprintf(command, sizeof(command),
             "/bin/chmod -R u+w '%s' && /bin/rm -rf '%s'", temporary,
             temporary);
    int removed = system(command) == 0;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: profile purge cases (wait status %d)\n",
                status);
        return 1;
    }
    assert(removed);
    puts("PASS: a purged profile leaves no profile behind, and an interrupted "
         "purge is finished by the next list or purge");
    return 0;
}
