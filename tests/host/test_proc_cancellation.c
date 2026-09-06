#include "util/proc.h"
#include <assert.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>
int main(void)
{
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
    puts("PASS: cancellation prevents new work while allowing bounded cleanup");
}
