/* Cancellation racing a supervised command's start. Wrapped pipe/fork calls
 * inject SIGTERM at the exact point of each window, so no sleep decides the
 * outcome:
 * - in the owner, after run_supervised's first cancellation check but before
 *   it blocks signals, when there is nothing to forward the signal to;
 * - in the supervisor, just before it forks the command, when a forwarded
 *   signal reaches a process group that does not contain the command yet.
 * A lost signal lets the command outlive a bounded alarm and exit 42. */
#include <assert.h>
#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static pid_t owner;
static int inject_before_block;
static int inject_before_command;
static int wrapped_pipe(int fds[2]);
static pid_t wrapped_fork(void);
#define pipe wrapped_pipe
#define fork wrapped_fork
#include "../../host/util/proc.c"
#undef fork
#undef pipe

static int wrapped_pipe(int fds[2])
{
    if (getpid() == owner && inject_before_block) {
        inject_before_block = 0;
        assert(raise(SIGTERM) == 0); /* only sets the cancellation flag */
    }
    return pipe(fds);
}

static pid_t wrapped_fork(void)
{
    /* The supervisor has blocked the signal: it stays pending until after
     * the command exists, which inherits no pending signal. */
    if (getpid() != owner && inject_before_command)
        assert(raise(SIGTERM) == 0);
    return fork();
}

static void linger_expired(int number)
{
    (void)number;
    _exit(42);
}

/* Command mode: runs until a signal ends it, or exits 42 after the alarm. */
static int linger(void)
{
    signal(SIGALRM, linger_expired);
    alarm(3);
    for (;;)
        pause();
}

static void no_children_remain(void)
{
    int status;
    errno = 0;
    assert(waitpid(-1, &status, WNOHANG) == -1 && errno == ECHILD);
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "linger") == 0)
        return linger();
    owner = getpid();
    const char *lingering[] = { argv[0], "linger", NULL };

    /* A signal recorded before the command existed still stops it, with and
     * without output capture. */
    inject_before_command = 1;
    assert(proc_run(lingering) == -1);
    char output[64];
    int truncated = 1;
    assert(proc_run_capture_checked(lingering, output, sizeof(output),
                                    &truncated) == -1);
    inject_before_command = 0;
    no_children_remain();

    /* A later supervisor starts without that signal. */
    const char *seven[] = { "/bin/sh", "-c", "exit 7", NULL };
    assert(proc_run(seven) == 7);
    no_children_remain();

    /* Cancellation is sticky, so the owner-side window runs in a child. */
    const char *tmp = getenv("TMPDIR");
    char directory[PATH_MAX];
    snprintf(directory, sizeof(directory), "%s/hamn-proc-race.XXXXXX",
             tmp && *tmp ? tmp : "/tmp");
    assert(mkdtemp(directory));
    char marker[PATH_MAX];
    snprintf(marker, sizeof(marker), "%s/started", directory);
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        owner = getpid();
        assert(proc_cancel_install() == 0);
        const char *touch[] = { "/usr/bin/touch", marker, NULL };
        inject_before_block = 1;
        errno = 0;
        int rc = proc_run(touch);
        assert(rc == 130 && errno == ECANCELED);
        assert(access(marker, F_OK) != 0 && errno == ENOENT);
        _exit(0);
    }
    int status;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(rmdir(directory) == 0);
    no_children_remain();
    puts("PASS: a cancellation racing a supervised command's start is never lost");
    return 0;
}
