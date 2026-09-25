/* vm_process_probe() adopts, and vm_stop() signals, a vmrun supervisor only
 * when its control-socket status reports the OS start token of the process
 * recorded in vmrun.pid/vmrun.identity, and that identity carries the
 * executable UUID. The supervisor here is a forked child
 * of this test (so it shares the test executable's UUID) that answers status
 * requests with scripted replies; no VM is involved. */
#include <assert.h>
#include <errno.h>
#include <inttypes.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#include "core/lifecycle.h"
#include "core/profile.h"
#include "util/proc.h"

enum reply {
    REPLY_START_IDENTITY,  /* what every vmrun since v0.0.1 reports */
    REPLY_NO_START,        /* pre-release vmrun: pid only */
    REPLY_SEC_ONLY,
    REPLY_ZERO_START,
    REPLY_OTHER_START,     /* the start token of another process */
};

#define MAX_REPLIES 4

struct supervisor {
    pid_t pid;
    uint64_t start_sec;
    uint64_t start_usec;
    unsigned char uuid[16];
    int alive_fd;  /* the test's end: its close tells the child to exit */
    int event_fd;  /* the child writes 'T' here when it receives SIGTERM */
};

static int g_event_fd = -1;

static void on_sigterm(int signal_number)
{
    (void)signal_number;
    char byte = 'T';
    (void)write(g_event_fd, &byte, 1);
    _exit(0);
}

static int reply_text(enum reply reply, char *text, size_t cap)
{
    uint64_t sec = 0, usec = 0;
    if (proc_start_identity(getpid(), &sec, &usec) != 0)
        return -1;
    int pid = (int)getpid();
    int n;
    switch (reply) {
    case REPLY_START_IDENTITY:
        n = snprintf(text, cap,
                     "{\"state\":\"running\",\"pid\":%d,\"start_sec\":%" PRIu64
                     ",\"start_usec\":%" PRIu64 "}\n", pid, sec, usec);
        break;
    case REPLY_NO_START:
        n = snprintf(text, cap, "{\"state\":\"running\",\"pid\":%d}\n", pid);
        break;
    case REPLY_SEC_ONLY:
        n = snprintf(text, cap,
                     "{\"state\":\"running\",\"pid\":%d,\"start_sec\":%" PRIu64
                     "}\n", pid, sec);
        break;
    case REPLY_ZERO_START:
        n = snprintf(text, cap,
                     "{\"state\":\"running\",\"pid\":%d,\"start_sec\":0,"
                     "\"start_usec\":0}\n", pid);
        break;
    case REPLY_OTHER_START:
        n = snprintf(text, cap,
                     "{\"state\":\"running\",\"pid\":%d,\"start_sec\":%" PRIu64
                     ",\"start_usec\":%" PRIu64 "}\n", pid, sec,
                     (usec + 1) % 1000000);
        break;
    default:
        return -1;
    }
    return n > 0 && (size_t)n < cap ? 0 : -1;
}

/* The child: serves `replies` in order (repeating the last) on vmrun.sock
 * until the test closes its end of `alive_fd`. Without replies it serves no
 * control socket, like vmrun before it creates one. */
static void serve(const struct profile *p, const enum reply *replies,
                  int reply_count, int alive_fd, int ready_fd)
{
    char ready = 'r';
    if (reply_count == 0) {
        if (write(ready_fd, &ready, 1) != 1)
            _exit(92);
        close(ready_fd);
        struct pollfd alive = { .fd = alive_fd, .events = POLLIN };
        while (poll(&alive, 1, -1) < 0) {
            if (errno != EINTR)
                _exit(93);
        }
        _exit(0);
    }
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    char path[1024];
    if (!profile_path(p, "vmrun.sock", path, sizeof(path)) ||
        strlen(path) >= sizeof(address.sun_path))
        _exit(90);
    snprintf(address.sun_path, sizeof(address.sun_path), "%s", path);
    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    if (listener < 0 ||
        bind(listener, (struct sockaddr *)&address, sizeof(address)) != 0 ||
        listen(listener, 4) != 0)
        _exit(91);
    if (write(ready_fd, &ready, 1) != 1)
        _exit(92);
    close(ready_fd);
    for (int served = 0;; served++) {
        struct pollfd fds[2] = {
            { .fd = listener, .events = POLLIN },
            { .fd = alive_fd, .events = POLLIN },
        };
        while (poll(fds, 2, -1) < 0) {
            if (errno != EINTR)
                _exit(93);
        }
        if (fds[1].revents)
            _exit(0);
        int client = accept(listener, NULL, NULL);
        if (client < 0)
            _exit(94);
        char request[512];
        ssize_t length = read(client, request, sizeof(request) - 1);
        char text[256];
        enum reply reply = replies[served < reply_count ? served :
                                   reply_count - 1];
        if (length <= 0 || reply_text(reply, text, sizeof(text)) != 0 ||
            write(client, text, strlen(text)) != (ssize_t)strlen(text))
            _exit(95);
        close(client);
    }
}

static void supervisor_start(const struct profile *p, const enum reply *replies,
                             int reply_count, struct supervisor *supervisor)
{
    assert(reply_count >= 0 && reply_count <= MAX_REPLIES);
    int alive[2], ready[2], events[2];
    assert(pipe(alive) == 0 && pipe(ready) == 0 && pipe(events) == 0);
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        close(alive[1]);
        close(ready[0]);
        close(events[0]);
        g_event_fd = events[1];
        if (signal(SIGTERM, on_sigterm) == SIG_ERR)
            _exit(96);
        serve(p, replies, reply_count, alive[0], ready[1]);
        _exit(97);
    }
    close(alive[0]);
    close(ready[1]);
    close(events[1]);
    struct pollfd wait_ready = { .fd = ready[0], .events = POLLIN };
    assert(poll(&wait_ready, 1, 10000) == 1);
    char byte = 0;
    assert(read(ready[0], &byte, 1) == 1 && byte == 'r');
    close(ready[0]);
    supervisor->pid = pid;
    supervisor->alive_fd = alive[1];
    supervisor->event_fd = events[0];
    assert(proc_start_identity(pid, &supervisor->start_sec,
                               &supervisor->start_usec) == 0);
    assert(proc_executable_identity(pid, supervisor->uuid) == 0);
}

/* Whether the child is still running, unreaped. */
static int supervisor_running(const struct supervisor *supervisor)
{
    int status;
    return waitpid(supervisor->pid, &status, WNOHANG) == 0;
}

/* Ends a child that must still be running and returns whether it had
 * received SIGTERM before (it reports that on the event pipe). */
static int supervisor_kill(struct supervisor *supervisor)
{
    assert(supervisor_running(supervisor));
    assert(kill(supervisor->pid, SIGKILL) == 0);
    int status;
    pid_t reaped;
    do {
        reaped = waitpid(supervisor->pid, &status, 0);
    } while (reaped < 0 && errno == EINTR);
    assert(reaped == supervisor->pid && WIFSIGNALED(status) &&
           WTERMSIG(status) == SIGKILL);
    close(supervisor->alive_fd);
    char byte = 0;
    ssize_t length = read(supervisor->event_fd, &byte, 1);
    close(supervisor->event_fd);
    return length == 1 && byte == 'T';
}

/* Waits for the SIGTERM report of a child that someone else (vm_stop) may
 * already have reaped. */
static int supervisor_terminated(struct supervisor *supervisor)
{
    struct pollfd event = { .fd = supervisor->event_fd, .events = POLLIN };
    assert(poll(&event, 1, 10000) == 1);
    char byte = 0;
    ssize_t length = read(supervisor->event_fd, &byte, 1);
    close(supervisor->event_fd);
    close(supervisor->alive_fd);
    int status;
    (void)waitpid(supervisor->pid, &status, 0);
    return length == 1 && byte == 'T';
}

static void write_text(const struct profile *p, const char *file,
                       const char *text)
{
    char path[1024];
    assert(profile_path(p, file, path, sizeof(path)));
    FILE *f = fopen(path, "w");
    assert(f && fputs(text, f) >= 0 && fclose(f) == 0);
}

static void read_text(const struct profile *p, const char *file, char *text,
                      size_t cap)
{
    char path[1024];
    assert(profile_path(p, file, path, sizeof(path)));
    FILE *f = fopen(path, "r");
    assert(f);
    size_t length = fread(text, 1, cap - 1, f);
    assert(!ferror(f) && fclose(f) == 0);
    text[length] = '\0';
}

static int exists(const struct profile *p, const char *file)
{
    char path[1024];
    struct stat sb;
    assert(profile_path(p, file, path, sizeof(path)));
    return lstat(path, &sb) == 0;
}

/* Records `supervisor` as the profile's vmrun, as vmrun itself does. */
static void write_identity(const struct profile *p,
                           const struct supervisor *supervisor)
{
    char uuid[33], text[160];
    proc_executable_uuid_format(supervisor->uuid, uuid);
    snprintf(text, sizeof(text), "%d %" PRIu64 " %" PRIu64 " %s\n",
             supervisor->pid, supervisor->start_sec, supervisor->start_usec,
             uuid);
    write_text(p, "vmrun.identity", text);
    snprintf(text, sizeof(text), "%d\n", supervisor->pid);
    write_text(p, "vmrun.pid", text);
}

/* The pre-release vmrun.identity form: no executable UUID. */
static void write_identity_without_uuid(const struct profile *p,
                                        const struct supervisor *supervisor)
{
    char text[160];
    snprintf(text, sizeof(text), "%d %" PRIu64 " %" PRIu64 "\n",
             supervisor->pid, supervisor->start_sec, supervisor->start_usec);
    write_text(p, "vmrun.identity", text);
}

static void profile_create(const char *root, const char *name,
                           struct profile *p)
{
    memset(p, 0, sizeof(*p));
    snprintf(p->name, sizeof(p->name), "%s", name);
    int n = snprintf(p->dir, sizeof(p->dir), "%s/%s", root, name);
    assert(n > 0 && n < (int)sizeof(p->dir) && mkdir(p->dir, 0700) == 0);
}

static void status_with_start_identity_is_verified(const char *root)
{
    struct profile p;
    profile_create(root, "verified", &p);
    const enum reply replies[] = { REPLY_START_IDENTITY };
    struct supervisor supervisor;
    supervisor_start(&p, replies, 1, &supervisor);
    write_identity(&p, &supervisor);
    int pid = -1;
    assert(vm_process_probe(&p, &pid) == VM_PROCESS_VERIFIED &&
           pid == supervisor.pid);
    assert(vm_running_pid(&p) == supervisor.pid);
    assert(!supervisor_kill(&supervisor));
}

/* The status lacks, zeroes or changes the start token of a supervisor whose
 * pid, start token and executable are all recorded: never the owned VM. */
static void status_without_matching_start_identity_is_unverified(
    const char *root)
{
    static const struct {
        const char *name;
        enum reply reply;
    } cases[] = {
        { "no-start", REPLY_NO_START },
        { "sec-only", REPLY_SEC_ONLY },
        { "zero-start", REPLY_ZERO_START },
        { "other-start", REPLY_OTHER_START },
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        struct profile p;
        profile_create(root, cases[i].name, &p);
        struct supervisor supervisor;
        supervisor_start(&p, &cases[i].reply, 1, &supervisor);
        write_identity(&p, &supervisor);
        char identity_before[160], identity_after[160];
        read_text(&p, "vmrun.identity", identity_before,
                  sizeof(identity_before));
        int pid = -1;
        assert(vm_process_probe(&p, &pid) == VM_PROCESS_UNVERIFIED &&
               pid == -1);
        assert(vm_running_pid(&p) == -1);
        /* Neither stale cleanup nor stop may act on it. */
        assert(vm_cleanup_stale(&p) == -1);
        int was_running = 1;
        assert(vm_stop(&p, &was_running) == -1 && was_running == 0);
        read_text(&p, "vmrun.identity", identity_after,
                  sizeof(identity_after));
        assert(strcmp(identity_before, identity_after) == 0);
        assert(exists(&p, "vmrun.pid") && exists(&p, "vmrun.sock"));
        assert(!supervisor_kill(&supervisor));
    }
}

/* vm_stop re-reads the status before it signals. A supervisor verified at
 * first is signaled only while the status still reports its start token. */
static void stop_signals_only_while_status_reports_start_identity(
    const char *root)
{
    struct profile p;
    profile_create(root, "stop-lost", &p);
    const enum reply lost[] = { REPLY_START_IDENTITY, REPLY_NO_START };
    struct supervisor supervisor;
    supervisor_start(&p, lost, 2, &supervisor);
    write_identity(&p, &supervisor);
    int was_running = 0;
    assert(vm_stop(&p, &was_running) == -1 && was_running == 1);
    assert(exists(&p, "vmrun.identity") && exists(&p, "vmrun.pid"));
    assert(!supervisor_kill(&supervisor));

    /* The same fixture with an unchanged status is stopped: the refusal
     * above came from the status, not from the fixture. */
    profile_create(root, "stop-kept", &p);
    const enum reply kept[] = { REPLY_START_IDENTITY };
    supervisor_start(&p, kept, 1, &supervisor);
    write_identity(&p, &supervisor);
    was_running = 0;
    (void)vm_stop(&p, &was_running);
    assert(was_running == 1);
    assert(supervisor_terminated(&supervisor));
    assert(!exists(&p, "vmrun.identity") && !exists(&p, "vmrun.pid"));
}

/* A live supervisor with a verified status, recorded by a vmrun.identity
 * without its executable UUID, is not adopted: the identity file is neither
 * trusted nor rewritten, and nothing signals or cleans up the process. */
static void identity_without_uuid_is_not_adopted_from_a_status(
    const char *root)
{
    struct profile p;
    profile_create(root, "no-uuid-ctl", &p);
    const enum reply replies[] = { REPLY_START_IDENTITY };
    struct supervisor supervisor;
    supervisor_start(&p, replies, 1, &supervisor);
    write_identity(&p, &supervisor);
    write_identity_without_uuid(&p, &supervisor);
    char before[160], after[160];
    read_text(&p, "vmrun.identity", before, sizeof(before));
    int pid = -1;
    assert(vm_process_probe(&p, &pid) == VM_PROCESS_UNVERIFIED && pid == -1);
    assert(vm_cleanup_stale(&p) == -1);
    int was_running = 1;
    assert(vm_stop(&p, &was_running) == -1 && was_running == 0);
    read_text(&p, "vmrun.identity", after, sizeof(after));
    assert(strcmp(before, after) == 0 && exists(&p, "vmrun.pid"));
    assert(!supervisor_kill(&supervisor));
}

/* Before vmrun serves its control socket, only a complete identity of the
 * live process verifies it. The same process recorded without its UUID is
 * unverified, like a corrupt identity, and is neither stopped nor cleaned. */
static void identity_without_uuid_is_unverified_before_the_control_socket(
    const char *root)
{
    struct profile p;
    profile_create(root, "pre-ctl-uuid", &p);
    struct supervisor supervisor;
    supervisor_start(&p, NULL, 0, &supervisor);
    write_identity(&p, &supervisor);
    int pid = -1;
    assert(vm_process_probe(&p, &pid) == VM_PROCESS_VERIFIED &&
           pid == supervisor.pid);
    assert(!supervisor_kill(&supervisor));

    profile_create(root, "pre-ctl-no-uuid", &p);
    supervisor_start(&p, NULL, 0, &supervisor);
    write_identity_without_uuid(&p, &supervisor);
    char before[160], after[160];
    read_text(&p, "vmrun.identity", before, sizeof(before));
    pid = -1;
    assert(vm_process_probe(&p, &pid) == VM_PROCESS_UNVERIFIED && pid == -1);
    assert(vm_cleanup_stale(&p) == -1);
    int was_running = 1;
    assert(vm_stop(&p, &was_running) == -1 && was_running == 0);
    read_text(&p, "vmrun.identity", after, sizeof(after));
    assert(strcmp(before, after) == 0);
    assert(!supervisor_kill(&supervisor));
}

/* A complete identity of a process that is gone is stale and cleaned up. An
 * identity without the UUID, like any unreadable identity, is unverified:
 * its files stay for inspection. PID 2147483646 exceeds macOS's PID range,
 * so no process can hold it. */
static void identity_without_uuid_of_a_gone_process_is_kept(const char *root)
{
    static const struct {
        const char *name;
        const char *identity;
        enum vm_process_state state;
    } cases[] = {
        { "gone-uuid",
          "2147483646 1 1 0123456789abcdef0123456789abcdef\n",
          VM_PROCESS_STALE },
        { "gone-no-uuid", "2147483646 1 1\n", VM_PROCESS_UNVERIFIED },
        { "gone-corrupt", "not an identity\n", VM_PROCESS_UNVERIFIED },
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        struct profile p;
        profile_create(root, cases[i].name, &p);
        write_text(&p, "vmrun.identity", cases[i].identity);
        write_text(&p, "vmrun.pid", "2147483646\n");
        assert(vm_process_probe(&p, NULL) == cases[i].state);
        if (cases[i].state == VM_PROCESS_STALE) {
            assert(vm_cleanup_stale(&p) == 0);
            assert(!exists(&p, "vmrun.identity") && !exists(&p, "vmrun.pid"));
        } else {
            char after[160];
            assert(vm_cleanup_stale(&p) == -1);
            read_text(&p, "vmrun.identity", after, sizeof(after));
            assert(strcmp(after, cases[i].identity) == 0);
            assert(exists(&p, "vmrun.pid"));
        }
    }
}

/* The cases run in a child, so a failed assertion still leaves this process
 * to remove the profiles; supervisors exit when that child's end of their
 * alive pipe closes. */
int main(void)
{
    char root[] = "/tmp/hamn-vmrun-identity-XXXXXX";
    assert(mkdtemp(root));
    pid_t runner = fork();
    assert(runner >= 0);
    if (runner == 0) {
        status_with_start_identity_is_verified(root);
        status_without_matching_start_identity_is_unverified(root);
        stop_signals_only_while_status_reports_start_identity(root);
        identity_without_uuid_is_not_adopted_from_a_status(root);
        identity_without_uuid_is_unverified_before_the_control_socket(root);
        identity_without_uuid_of_a_gone_process_is_kept(root);
        _exit(0);
    }
    int status = 0;
    pid_t reaped;
    do {
        reaped = waitpid(runner, &status, 0);
    } while (reaped < 0 && errno == EINTR);

    char command[sizeof(root) + 16];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", root);
    int removed = system(command) == 0;
    if (reaped != runner || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: vmrun identity cases (wait status %d)\n",
                status);
        return 1;
    }
    assert(removed);
    puts("PASS: vmrun is adopted or signaled only with its complete recorded "
         "and reported identity");
    return 0;
}
