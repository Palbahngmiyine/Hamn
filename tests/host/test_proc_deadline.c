#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "util/proc.h"

static int completed(int rc, void *context)
{
    return proc_write_all(*(int *)context, &rc, sizeof(rc));
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "block") == 0) {
        signal(SIGTERM, SIG_IGN);
        printf("%d\n", getpid());
        fflush(stdout);
        for (;;) pause();
    }
    if (argc == 2 && strcmp(argv[1], "silent") == 0) {
        for (;;) pause();
    }
    char output[128];
    const char *success[] = {"/bin/sh", "-c", "printf ready; exit 7", NULL};
    assert(proc_run_bounded(success, output, sizeof(output), 5000, NULL, NULL) == 7);
    assert(strcmp(output, "ready") == 0);
    assert(proc_run_bounded(success, output, sizeof(output), 0, NULL, NULL) == -1);
    assert(errno == EINVAL);
    const char *blocked[] = {argv[0], "block", NULL};
    int notify[2];
    assert(proc_pipe_cloexec(notify) == 0);
    assert(proc_run_bounded(blocked, output, sizeof(output), 1000,
                            completed, &notify[1]) == PROC_RUN_TIMEOUT);
    int pid = atoi(output), result;
    assert(pid > 0);
    assert(kill(pid, 0) == -1 && errno == ESRCH); /* child was reaped */
    close(notify[1]);
    assert(proc_read_all(notify[0], &result, sizeof(result)) == 0);
    assert(result == PROC_RUN_TIMEOUT);
    assert(read(notify[0], &result, sizeof(result)) == 0); /* exactly once */
    close(notify[0]);
    const char *silent[] = {argv[0], "silent", NULL};
    assert(proc_run_bounded(silent, NULL, 0, 100, NULL, NULL) == PROC_RUN_TIMEOUT);
    const char *absent[] = {"/hamn-no-such-command", NULL};
    assert(proc_run_bounded(absent, output, sizeof(output), 5000, NULL, NULL) == 127);
    puts("PASS: process deadlines reap blocked children and retain completion semantics");
    return 0;
}
