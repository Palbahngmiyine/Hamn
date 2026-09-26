#include "util/proc.h"
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

/* Command mode modelling an ssh multiplexing client: its descendant (the
 * ControlMaster's role) keeps the captured stdout open and ignores the
 * forwarded cancellation. The descendant exits when the test closes the
 * release pipe, or after a safety alarm that bounds the original defect. */
static int hold_capture(pid_t owner, int release_fd)
{
    int ready[2];
    if (pipe(ready) != 0)
        return 2;
    pid_t holder = fork();
    if (holder < 0)
        return 2;
    if (holder == 0) {
        signal(SIGINT, SIG_IGN);
        signal(SIGTERM, SIG_IGN);
        signal(SIGHUP, SIG_IGN);
        alarm(10);
        close(ready[0]);
        if (write(ready[1], "1", 1) != 1)
            _exit(2);
        close(ready[1]);
        char byte;
        while (read(release_fd, &byte, 1) < 0 && errno == EINTR)
            ;
        _exit(0);
    }
    close(ready[1]);
    char byte;
    if (read(ready[0], &byte, 1) != 1 || kill(owner, SIGINT) != 0)
        return 2;
    for (;;)
        pause(); /* the forwarded SIGINT terminates this exact child */
}

/* Regression: cancelling a captured command used to wait until every
 * descendant released the capture pipe, i.e. until the remote command ended. */
static void cancelled_capture_does_not_wait_for_descendants(const char *self)
{
    int release[2], alive[2];
    assert(pipe(release) == 0 && pipe(alive) == 0);
    /* Only the holder may keep alive[1]; only this process keeps release[1]. */
    assert(fcntl(release[1], F_SETFD, FD_CLOEXEC) == 0);
    assert(fcntl(alive[0], F_SETFD, FD_CLOEXEC) == 0);
    char owner[32], release_fd[32];
    snprintf(owner, sizeof(owner), "%d", (int)getpid());
    snprintf(release_fd, sizeof(release_fd), "%d", release[0]);
    const char *command[] = { self, "hold-capture", owner, release_fd, NULL };
    char output[64];
    int truncated = 0;
    assert(proc_run_capture_checked(command, output, sizeof(output),
                                    &truncated) == -1);
    assert(proc_cancelled());
    close(release[0]);
    close(alive[1]);
    /* The call returned while the descendant still held the capture pipe. */
    struct pollfd holder = { .fd = alive[0], .events = POLLIN };
    assert(poll(&holder, 1, 0) == 0);
    int status;
    errno = 0;
    assert(waitpid(-1, &status, WNOHANG) == -1 && errno == ECHILD);
    close(release[1]);
    char byte;
    assert(read(alive[0], &byte, 1) == 0); /* the released holder exits */
    close(alive[0]);
}

int main(int argc, char **argv)
{
    if (argc == 4 && strcmp(argv[1], "hold-capture") == 0)
        return hold_capture((pid_t)atoi(argv[2]), atoi(argv[3]));
    /* Cancellation cannot be reset, so the capture case owns a child process. */
    pid_t capture_case = fork();
    assert(capture_case >= 0);
    if (capture_case == 0) {
        assert(proc_cancel_install() == 0);
        cancelled_capture_does_not_wait_for_descendants(argv[0]);
        _exit(0);
    }
    int status;
    assert(waitpid(capture_case, &status, 0) == capture_case);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);

    const char *command[] = {"/usr/bin/true", NULL};
    assert(proc_cancel_install() == 0);
    assert(!proc_cancelled());
    assert(raise(SIGTERM) == 0);
    assert(proc_cancelled());
    assert(proc_run(command) == 130);
    assert(proc_spawn_daemon(command, "/dev/null") == -1);
    proc_cleanup_begin();
    assert(!proc_cancelled());
    assert(proc_run_bounded(command, NULL, 0, 1000, NULL, NULL) == 0);
    proc_cleanup_begin(); proc_cleanup_end();
    assert(!proc_cancelled());
    proc_cleanup_end();
    assert(proc_cancelled());
    assert(proc_run(command) == 130);
    puts("PASS: cancellation stops capture at the exact child, prevents new work, and allows bounded cleanup");
}
