/* vmrun's watch of an ephemeral profile's workspace (cmd_vmrun.c): the first
 * check that finds the workspace absent asks the guest to stop, the checks
 * end there, and at exit, once its identity files are gone, vmrun starts
 * one "workspace-reclaim" with the record it watched. A workspace that is
 * there, or that cannot be looked up, keeps the VM; a profile without a
 * record is not watched; a gone workspace found while a stop is under way
 * does not force that stop.
 *
 * cmd_vmrun.c is compiled into this test a second time (Makefile) with a
 * check every 50 ms and these functions renamed to the fakes below:
 * Virtualization.framework's VM, the detached spawn, and workspace_presence,
 * which still asks the real one and reports each answer. Each fake writes
 * one byte to a pipe that the test reads, so cases wait for counted events,
 * not for time, except to show that something does not happen. */
#include <assert.h>
#include <dispatch/dispatch.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "cli.h"
#include "core/profile.h"
#include "core/workspace.h"
#include "vz/vz_shim.h"

/* Events: V the VM started, p a or u a check found the workspace present,
 * absent or unknown, S a guest stop was requested, F the VM was forced off,
 * R the reclaim was started. */
static int events = -1;
static int stop_delay_ms;
static const char *identity_file;
static char spawned[PATH_MAX];
static vz_state_cb state_callback;
static void *state_context;

static void event(char kind)
{
    assert(write(events, &kind, 1) == 1);
}

vz_vm *fake_vz_vm_create(const vz_vm_spec *spec, vz_state_cb cb, void *ud,
                         char **err)
{
    (void)spec;
    (void)err;
    state_callback = cb;
    state_context = ud;
    return (vz_vm *)&state_callback;
}

int fake_vz_vm_start(vz_vm *vm, char **err)
{
    (void)vm;
    (void)err;
    event('V');
    return 0;
}

int fake_vz_vm_request_stop(vz_vm *vm, char **err)
{
    (void)vm;
    (void)err;
    event('S');
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,
                                 (int64_t)stop_delay_ms * NSEC_PER_MSEC),
                   dispatch_get_main_queue(), ^{
                       state_callback(state_context, VZ_ST_STOPPED);
                   });
    return 0;
}

int fake_vz_vm_force_stop(vz_vm *vm, char **err)
{
    (void)vm;
    (void)err;
    event('F');
    return 0;
}

void fake_vz_runloop(void)
{
    dispatch_main();
}

/* Records what would run, NUL-separated: the mode, the record, the log.
 * vmrun's identity is gone by then, so that the purge finds its VM
 * stopped. */
pid_t fake_proc_spawn_daemon(const char *const argv[], const char *logfile)
{
    assert(!identity_file || access(identity_file, F_OK) != 0);
    assert(argv[0][0] == '/' && argv[1] && argv[2] && !argv[3]);
    int fd = open(spawned, O_WRONLY | O_CREAT | O_EXCL, 0600);
    assert(fd >= 0);
    for (const char *const *part = (const char *const[]){ argv[1], argv[2],
             logfile, NULL };
         *part; part++)
        assert(write(fd, *part, strlen(*part) + 1) ==
               (ssize_t)strlen(*part) + 1);
    assert(close(fd) == 0);
    event('R');
    return 4242;
}

enum workspace_presence fake_workspace_presence(const char *path)
{
    enum workspace_presence presence = workspace_presence(path);
    event(presence == WORKSPACE_PRESENT ? 'p' :
          presence == WORKSPACE_ABSENT ? 'a' : 'u');
    return presence;
}

static char root[PATH_MAX];

static const char *at(const char *name)
{
    static char path[2][PATH_MAX];
    static int next;
    next = !next;
    snprintf(path[next], sizeof(path[next]), "%s/%s", root, name);
    return path[next];
}

static void write_file(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    size_t length = strlen(text);
    assert(fd >= 0 && write(fd, text, length) == (ssize_t)length &&
           close(fd) == 0);
}

/* A running vmrun of one case. */
struct run {
    pid_t pid;
    int events;
    char seen[256];
    size_t count;
};

/* With identity, vmrun writes its identity files as vm start has it do. */
static void start(struct run *run, const char *profile, int delay_ms,
                  int identity)
{
    int pipe_fds[2];
    assert(pipe(pipe_fds) == 0);
    snprintf(spawned, sizeof(spawned), "%s", at("spawned"));
    (void)unlink(spawned);
    memset(run, 0, sizeof(*run));
    run->pid = fork();
    assert(run->pid >= 0);
    if (run->pid == 0) {
        close(pipe_fds[0]);
        events = pipe_fds[1];
        stop_delay_ms = delay_ms;
        alarm(20);
        char pidfile[PATH_MAX], identity_path[PATH_MAX];
        snprintf(pidfile, sizeof(pidfile), "%s", at("vmrun.pid"));
        snprintf(identity_path, sizeof(identity_path), "%s",
                 at("vmrun.identity"));
        char *argv[] = { "vmrun", "--disk", (char *)at("disk.img"),
                         "--profile", (char *)profile, "--pidfile", pidfile,
                         "--identity-file", identity_path, NULL };
        if (identity)
            identity_file = identity_path;
        cmd_vmrun(identity ? 9 : 5, argv);
        _exit(99);
    }
    close(pipe_fds[1]);
    run->events = pipe_fds[0];
}

/* The next event within 10 s; 0 at the end of the run, -1 at the deadline. */
static int next_event(struct run *run, int timeout_ms)
{
    struct pollfd poller = { .fd = run->events, .events = POLLIN };
    int ready = poll(&poller, 1, timeout_ms);
    assert(ready >= 0);
    if (ready == 0)
        return -1;
    char kind;
    ssize_t n = read(run->events, &kind, 1);
    assert(n >= 0);
    if (n == 0)
        return 0;
    assert(run->count < sizeof(run->seen) - 1);
    run->seen[run->count++] = kind;
    return kind;
}

static void wait_for(struct run *run, char kind)
{
    int got;
    do {
        got = next_event(run, 10000);
        assert(got > 0);
    } while (got != kind);
}

/* Reads the remaining events and returns the exit status. */
static int finish(struct run *run)
{
    while (next_event(run, 10000) > 0) {
    }
    close(run->events);
    int status = 0;
    pid_t reaped;
    do {
        reaped = waitpid(run->pid, &status, 0);
    } while (reaped < 0 && errno == EINTR);
    assert(reaped == run->pid);
    return status;
}

static size_t occurrences(const struct run *run, char kind)
{
    size_t total = 0;
    for (size_t index = 0; index < run->count; index++)
        total += run->seen[index] == kind;
    return total;
}

/* An ephemeral profile of HOME/.hamn whose record names the directory
 * tree; its record into record. */
static void ephemeral(const char *tree, char record[WORKSPACE_RECORD_CAP],
                      char profile_name[PROFILE_NAME_CAP])
{
    assert(mkdir(tree, 0700) == 0);
    struct workspace workspace;
    assert(workspace_identify(tree, &workspace) == 0);
    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.name, sizeof(profile.name), "%s", workspace.profile);
    assert(profile_locate(&profile) == 0 && mkdir(profile.dir, 0700) == 0);
    assert(workspace_record_write(&profile, &workspace) == 0);
    assert(workspace_record_format(&workspace, record) > 0);
    snprintf(profile_name, PROFILE_NAME_CAP, "%s", workspace.profile);
}

static void a_gone_workspace_stops_the_vm_once_and_starts_one_reclaim(void)
{
    char record[WORKSPACE_RECORD_CAP], name[PROFILE_NAME_CAP];
    ephemeral(at("gone"), record, name);
    struct run run;
    /* The stop takes several checks long, so that a check after the first
     * absent one would be seen. */
    start(&run, name, 400, 1);
    wait_for(&run, 'p');
    assert(rmdir(at("gone")) == 0);
    wait_for(&run, 'a');
    int status = finish(&run);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(occurrences(&run, 'a') == 1 && occurrences(&run, 'S') == 1 &&
           occurrences(&run, 'F') == 0 && occurrences(&run, 'R') == 1);
    const char *after = strchr(run.seen, 'a');
    assert(!strpbrk(after + 1, "pau"));

    char text[3 * WORKSPACE_RECORD_CAP], expected_log[PATH_MAX];
    int fd = open(spawned, O_RDONLY);
    ssize_t length = read(fd, text, sizeof(text));
    assert(fd >= 0 && length > 0 && close(fd) == 0);
    snprintf(expected_log, sizeof(expected_log),
             "%s/home/.hamn/%s/logs/vmrun.log", root, name);
    const char *mode = text, *argument = mode + strlen(mode) + 1,
               *log = argument + strlen(argument) + 1;
    assert(strcmp(mode, "workspace-reclaim") == 0);
    assert(strcmp(argument, record) == 0);
    assert(strcmp(log, expected_log) == 0);
    assert(log + strlen(log) + 1 == text + length);
    assert(access(at("vmrun.identity"), F_OK) != 0 &&
           access(at("vmrun.pid"), F_OK) != 0);
}

static void a_present_workspace_keeps_the_vm(void)
{
    char record[WORKSPACE_RECORD_CAP], name[PROFILE_NAME_CAP];
    ephemeral(at("present"), record, name);
    struct run run;
    start(&run, name, 0, 0);
    for (int check = 0; check < 3; check++)
        wait_for(&run, 'p');
    assert(kill(run.pid, SIGTERM) == 0);
    int status = finish(&run);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(occurrences(&run, 'a') == 0 && occurrences(&run, 'R') == 0 &&
           occurrences(&run, 'S') == 1);
    assert(access(spawned, F_OK) != 0);
}

/* EACCES is not "gone": a workspace that cannot be looked up stays. */
static void a_workspace_that_cannot_be_looked_up_keeps_the_vm(void)
{
    if (geteuid() == 0)
        return;
    assert(mkdir(at("sealed"), 0700) == 0);
    char record[WORKSPACE_RECORD_CAP], name[PROFILE_NAME_CAP];
    ephemeral(at("sealed/tree"), record, name);
    assert(chmod(at("sealed"), 0) == 0);
    struct run run;
    start(&run, name, 0, 0);
    for (int check = 0; check < 3; check++)
        wait_for(&run, 'u');
    assert(occurrences(&run, 'S') == 0);
    assert(kill(run.pid, SIGTERM) == 0);
    int status = finish(&run);
    assert(chmod(at("sealed"), 0700) == 0);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(occurrences(&run, 'R') == 0 && occurrences(&run, 'a') == 0);
}

static void a_profile_without_a_record_is_not_watched(void)
{
    assert(mkdir(at("home/.hamn/plain"), 0700) == 0);
    struct run run;
    start(&run, "plain", 0, 0);
    wait_for(&run, 'V');
    /* Ten intervals without a check. */
    assert(next_event(&run, 500) == -1);
    assert(kill(run.pid, SIGTERM) == 0);
    int status = finish(&run);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(!strpbrk(run.seen, "pauR"));
}

/* A stop under way, here from SIGTERM, ends vmrun anyway: the gone
 * workspace only adds the reclaim, and does not force the stop. */
static void a_workspace_gone_during_a_stop_adds_the_reclaim_only(void)
{
    char record[WORKSPACE_RECORD_CAP], name[PROFILE_NAME_CAP];
    ephemeral(at("leaving"), record, name);
    struct run run;
    start(&run, name, 600, 0);
    wait_for(&run, 'p');
    assert(kill(run.pid, SIGTERM) == 0);
    wait_for(&run, 'S');
    assert(rmdir(at("leaving")) == 0);
    wait_for(&run, 'a');
    int status = finish(&run);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(occurrences(&run, 'S') == 1 && occurrences(&run, 'F') == 0 &&
           occurrences(&run, 'R') == 1);
}

static void a_record_that_cannot_be_read_stops_vmrun_before_the_vm(void)
{
    assert(mkdir(at("home/.hamn/ws-000000000000"), 0700) == 0);
    write_file(at("home/.hamn/ws-000000000000/ephemeral"), "not a record\n");
    struct run run;
    start(&run, "ws-000000000000", 0, 0);
    int status = finish(&run);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 1);
    assert(run.count == 0);
}

int main(void)
{
    char temporary[] = "/tmp/hamn-vmrun-workspace-XXXXXX";
    assert(mkdtemp(temporary) && realpath(temporary, root));
    assert(mkdir(at("home"), 0700) == 0 && mkdir(at("home/.hamn"), 0700) == 0);
    assert(setenv("HOME", at("home"), 1) == 0);
    a_gone_workspace_stops_the_vm_once_and_starts_one_reclaim();
    a_present_workspace_keeps_the_vm();
    a_workspace_that_cannot_be_looked_up_keeps_the_vm();
    a_profile_without_a_record_is_not_watched();
    a_workspace_gone_during_a_stop_adds_the_reclaim_only();
    a_record_that_cannot_be_read_stops_vmrun_before_the_vm();
    char command[PATH_MAX + 32];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", root);
    if (system(command) != 0) {
        fprintf(stderr, "FAIL: cannot remove %s\n", root);
        return 1;
    }
    printf("vmrun workspace: all cases passed\n");
    return 0;
}
