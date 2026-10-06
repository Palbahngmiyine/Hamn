/* A supervised command under the two schedules its owner cannot choose:
 * - the supervisor has already exited when the owner first reads from it;
 * - the kernel refuses a setpgid the owner makes for the supervisor. macOS
 *   fails one of two racing setpgid calls for the same child with EPERM, and
 *   the failed call can return before the other has taken effect. Every owner
 *   call is answered that way here.
 * All scheduling uses waitid(WNOWAIT), not sleeps or probabilistic stress. */
#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int scenario, parent_signals, supervisor_exit_observed;
static pid_t owner;
static int racing_setpgid(pid_t pid, pid_t group);
static int checked_kill(pid_t pid, int number);
static ssize_t late_read(int fd, void *buffer, size_t size);
#define setpgid racing_setpgid
#define kill checked_kill
#define read late_read
#include "../../host/util/proc.c"
#undef read
#undef kill
#undef setpgid

static int racing_setpgid(pid_t pid, pid_t group)
{
    if (pid == 0) {
        if (scenario == 3) _exit(0); /* success exit with no protocol result */
        if (scenario == 4) _exit(1); /* supervisor setup failure */
        if (scenario == 5) raise(SIGKILL);
        return setpgid(pid, group);
    }
    errno = EPERM;
    return -1;
}

/* The owner's first read waits until its only child, the supervisor, has
 * exited. The exit is observed without reaping, so the child keeps its PID. */
static ssize_t late_read(int fd, void *buffer, size_t size)
{
    if (getpid() == owner && !supervisor_exit_observed) {
        siginfo_t info = {0};
        assert(waitid(P_ALL, 0, &info, WEXITED | WNOWAIT) == 0);
        assert(info.si_pid > 0);
        supervisor_exit_observed = 1;
    }
    return read(fd, buffer, size);
}

static int checked_kill(pid_t pid, int number)
{
    if (getpid() == owner) parent_signals++;
    return kill(pid, number);
}

int main(void)
{
    owner = getpid();
    for (scenario = 0; scenario <= 6; scenario++) {
        const char *command[] = { "/bin/sh", "-c", scenario == 1 ? "exit 7" :
                                  scenario == 2 ? "printf capture-proof" :
                                  scenario == 6 ? "ps -o pgid= -p $$" : "exit 0", NULL };
        char output[64] = {0};
        int captured = scenario == 2 || scenario == 6;
        supervisor_exit_observed = 0;
        int rc = captured ? proc_run_capture(command, output, sizeof(output)) : proc_run(command);
        assert(rc == (scenario == 1 ? 7 : scenario >= 3 && scenario <= 5 ? -1 : 0));
        assert(supervisor_exit_observed);
        if (scenario == 2) assert(!strcmp(output, "capture-proof"));
        if (scenario == 6) {
            /* The command ran in its supervisor's group, not the owner's. */
            long group = strtol(output, NULL, 10);
            assert(group > 0 && group != (long)getpgrp());
        }
        assert(parent_signals == 0); /* never signal an exited or unproven PID */
        int status;
        errno = 0;
        assert(waitpid(-1, &status, WNOHANG) == -1 && errno == ECHILD);
    }
    puts("PASS: early supervisor exit and a refused owner setpgid preserve command results; a missing protocol result is rejected");
}
