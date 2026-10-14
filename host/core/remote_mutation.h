#ifndef HAMN_REMOTE_MUTATION_H
#define HAMN_REMOTE_MUTATION_H
#include "core/profile.h"
/* Run a guest writer under the guest deployment lock with a deadline. The
 * guest ends it within wait_seconds + run_seconds + 25 s; its SSH client is
 * reaped SSH_REMOTE_MARGIN_SEC later, and the call then returns
 * PROC_RUN_TIMEOUT. Any unsuccessful call, that one included, leaves the
 * writer's outcome unknown: it fences even a delayed SSH dispatch before
 * waiting for a writer already inside its critical section. The writer's
 * output stays on stdout/stderr, or with output, what was captured (also on
 * failure) is there and *truncated says whether it was cut. */
int remote_mutation_run(const struct profile *profile, const char *ip,
                        unsigned wait_seconds, unsigned run_seconds,
                        const char *const command[],
                        char *output, size_t capacity, int *truncated);
/* Sticky for this one-operation worker: failed fencing must preserve the VM. */
int remote_mutation_cleanup_pending(void);
#endif
