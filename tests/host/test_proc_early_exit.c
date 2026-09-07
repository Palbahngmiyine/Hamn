/* Force the supervisor to exit before its parent sets the process group.
 * All scheduling uses waitid(WNOWAIT), not sleeps or probabilistic stress. */
#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int scenario, parent_signals;
static pid_t owner;
static int delayed_setpgid(pid_t pid, pid_t group);
static int checked_kill(pid_t pid, int number);
static int proof_waitid(idtype_t type, id_t id, siginfo_t *info, int options);
#define setpgid delayed_setpgid
#define kill checked_kill
#define waitid proof_waitid
#include "../../host/util/proc.c"
#undef waitid
#undef kill
#undef setpgid

static int delayed_setpgid(pid_t pid, pid_t group)
{
    if (pid == 0) {
        if (scenario == 3) _exit(0); /* success exit with no protocol result */
        if (scenario == 4) _exit(1); /* supervisor setup failure */
        if (scenario == 5) raise(SIGKILL);
        return setpgid(pid, group);
    }
    siginfo_t info = {0};
    assert(waitid(P_PID, (id_t)pid, &info, WEXITED | WNOWAIT) == 0);
    assert(info.si_pid == pid);
    int result = setpgid(pid, group);
    assert(result == -1 && errno == ESRCH);
    return result;
}

static int proof_waitid(idtype_t type, id_t id, siginfo_t *info, int options)
{
    assert(type == P_PID && (options & (WNOWAIT | WNOHANG)) == (WNOWAIT | WNOHANG));
    if (scenario == 6) { errno = ECHILD; return -1; }
    return waitid(type, id, info, options);
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
                                  scenario == 2 ? "printf capture-proof" : "exit 0", NULL };
        char output[64] = {0};
        int rc = scenario == 2 ? proc_run_capture(command, output, sizeof(output)) : proc_run(command);
        assert(rc == (scenario == 1 ? 7 : scenario >= 3 ? -1 : 0));
        if (scenario == 2) assert(!strcmp(output, "capture-proof"));
        assert(parent_signals == 0); /* never signal an exited or unproven PID */
        int status;
        errno = 0;
        assert(waitpid(-1, &status, WNOHANG) == -1 && errno == ECHILD);
    }
    puts("PASS: early supervisor exit preserves command results and rejects missing proof/protocol");
}
