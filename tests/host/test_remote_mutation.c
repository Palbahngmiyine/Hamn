#include "core/remote_mutation.h"
#include <assert.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int calls, cancelled, depth, fault, phase, held, fenced;
static char token[33];
int proc_cancelled(void) { return cancelled && !depth; }
void proc_cleanup_begin(void) { depth++; }
void proc_cleanup_end(void) { assert(depth > 0); depth--; }
int operation_phase(const char *value)
{ assert(!strcmp(value, "fencing-after-cancel")); phase++; return 0; }
void logerr(const char *format, ...) { (void)format; }

int ssh_exec(const struct profile *profile, const char *ip,
             const char *const argv[], int quiet)
{
    (void)profile; (void)ip; (void)quiet;
    calls++;
    if (calls == 1) {
        assert(!strcmp(argv[1], "timeout") && !strcmp(argv[4], "bash"));
        assert(strlen(argv[8]) == 32 && strspn(argv[8], "0123456789abcdef") == 32);
        strcpy(token, argv[8]);
        assert(strstr(argv[6], "if test -e") && strstr(argv[6], "exit 130"));
        if (fault == 0) return 0;
        held = fault == 1; /* fault 2: original invocation is still queued */
        cancelled = 1;
        return 130;
    }
    assert(depth == 1);
    if (calls == 2) {
        assert(!strcmp(argv[1], "bash") && !strcmp(argv[5], token));
        assert(strstr(argv[3], "test ! -L") && strstr(argv[3], "0:700"));
        if (fault == 3) return 1;
        fenced = 1;
        return 0;
    }
    assert(calls == 3 && fenced); /* publication must precede the barrier */
    assert(!strcmp(argv[4], "flock") && !strcmp(argv[8], "true"));
    if (fault == 4) return 1;
    held = 0;
    return 0;
}

int ssh_exec_capture_checked(const struct profile *p, const char *ip,
                             const char *const argv[], char *out,
                             size_t capacity, int *truncated)
{
    assert(capacity > 1); strcpy(out, "x"); *truncated = 0;
    return ssh_exec(p, ip, argv, 0);
}

int main(void)
{
    for (int scenario = 0; scenario < 5; scenario++) {
        pid_t child = fork(); assert(child >= 0);
        if (child == 0) {
            fault = scenario;
            struct profile p = {0};
            const char *command[] = { "sudo", "true", NULL };
            int result = remote_mutation_run(&p, "192.0.2.1", "/run/hamn-deployment.lock",
                                              120, 60, command, NULL, 0, NULL);
            assert(result == (fault ? 130 : 0) && depth == 0 && !held);
            assert(calls == (fault == 0 ? 1 : fault == 3 ? 2 : 3));
            assert(phase == !!fault);
            assert(remote_mutation_cleanup_pending() == (fault >= 3));
            if (fault == 2) assert(fenced); /* delayed original must see its fence */
            _exit(0);
        }
        int status; assert(waitpid(child, &status, 0) == child);
        assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    }
    puts("PASS: mutation token fence precedes lock barrier; unknown cleanup remains sticky");
}
