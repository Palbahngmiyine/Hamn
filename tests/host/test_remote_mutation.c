#include "core/remote_mutation.h"
#include "util/proc.h"
#include <assert.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int calls, cancelled, depth, fault, phase, held, fenced, failure_code, capture;
static char token[33];
int proc_cancelled(void) { return cancelled && !depth; }
void proc_cleanup_begin(void) { depth++; }
void proc_cleanup_end(void) { assert(depth > 0); depth--; }
int operation_phase(const char *value)
{ assert(!strcmp(value, failure_code == 130 ? "fencing-after-cancel" : "fencing-after-failure")); phase++; return 0; }
void logerr(const char *format, ...) { (void)format; }

/* The guest connection: the writer, then the fence, then the barrier. */
static int fake_ssh(const char *const argv[])
{
    calls++;
    if (calls == 1) {
        assert(!strcmp(argv[1], "timeout") && !strcmp(argv[4], "bash"));
        assert(!strcmp(argv[2], "--kill-after=5s"));
        assert(strlen(argv[8]) == 32 && strspn(argv[8], "0123456789abcdef") == 32);
        strcpy(token, argv[8]);
        assert(strstr(argv[6], "if test -e") && strstr(argv[6], "exit 130"));
        if (fault == 0) return 0;
        held = fault == 1; /* fault 2: original invocation is still queued */
        cancelled = failure_code == 130;
        return failure_code;
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

/* The guest ends a writer by flock's wait, its run limit and 20 s for sudo,
 * bash and the marker check, and 5 s after TERM at the latest. Its client is
 * reaped 10 s after that. */
static unsigned writer_client_ms(unsigned wait, unsigned run)
{
    return (wait + run + 20 + 5 + 10) * 1000;
}

int ssh_exec_bounded_shown(const struct profile *p, const char *ip,
                           const char *const argv[], unsigned timeout_ms)
{
    (void)p; (void)ip;
    assert(!capture && calls == 0 && timeout_ms == writer_client_ms(120, 60));
    return fake_ssh(argv);
}

int ssh_exec_capture_bounded(const struct profile *p, const char *ip,
                             const char *const argv[], char *out,
                             size_t capacity, int *truncated,
                             unsigned timeout_ms)
{
    (void)p; (void)ip;
    assert(capture && calls == 0 && timeout_ms == writer_client_ms(120, 120));
    assert(capacity > 1 && truncated); strcpy(out, "x"); *truncated = 1;
    return fake_ssh(argv);
}

/* The fence gets 30 s; the barrier flock's wait (120 s) and 10 s more. */
int ssh_exec_bounded(const struct profile *p, const char *ip,
                     const char *const argv[], unsigned timeout_ms)
{
    (void)p; (void)ip;
    assert(timeout_ms == (calls == 1 ? 30000 : 130000));
    return fake_ssh(argv);
}

/* One writer whose client ends with code in the given fault scenario, run in
 * a child so that each run starts from fresh module state. */
static void run_writer(int captured, int code, int scenario)
{
    pid_t child = fork(); assert(child >= 0);
    if (child == 0) {
        fault = scenario; failure_code = code; capture = captured;
        struct profile p = {0};
        const char *command[] = { "sudo", "true", NULL };
        char reason[16] = "";
        int truncated = 0;
        int result = capture ?
            remote_mutation_run(&p, "192.0.2.1", 120, 120, command, reason, sizeof(reason), &truncated) :
            remote_mutation_run(&p, "192.0.2.1", 120, 60, command, NULL, 0, NULL);
        assert(result == (fault ? failure_code : 0) && depth == 0 && !held);
        /* What was captured is kept, also from a failed writer. */
        if (capture) assert(!strcmp(reason, "x") && truncated == 1);
        assert(calls == (fault == 0 ? 1 : fault == 3 ? 2 : 3));
        assert(phase == !!fault);
        assert(remote_mutation_cleanup_pending() == (fault >= 3));
        if (fault >= 3) {
            int prior_calls = calls;
            assert(remote_mutation_run(&p, "192.0.2.1", 120, 60, command, NULL, 0, NULL) == -1);
            assert(calls == prior_calls); /* no rollback overtakes an unverified writer */
        }
        if (fault == 2) assert(fenced); /* delayed original must see its fence */
        _exit(0);
    }
    int status; assert(waitpid(child, &status, 0) == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

int main(void)
{
    /* PROC_RUN_TIMEOUT is both the guest's timeout and a reaped client. */
    const int failures[] = {130, 255, -1, PROC_RUN_TIMEOUT, 1};
    for (int captured = 0; captured < 2; captured++)
        for (size_t code = 0; code < sizeof(failures) / sizeof(failures[0]); code++)
            for (int scenario = 0; scenario < 5; scenario++)
                run_writer(captured, failures[code], scenario);
    puts("PASS: a bounded writer's cancellation, transport, timeout or command failure fences before the barrier; unknown cleanup blocks later writes");
}
