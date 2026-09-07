#ifndef HAMN_REMOTE_MUTATION_H
#define HAMN_REMOTE_MUTATION_H
#include "core/profile.h"
/* Lock and timeout the guest writer. Any unsuccessful call fences even a delayed
 * SSH dispatch before waiting for a writer already inside its critical section. */
int remote_mutation_run(const struct profile *profile, const char *ip,
                        const char *lock, unsigned wait_seconds,
                        unsigned run_seconds, const char *const command[],
                        char *output, size_t capacity, int *truncated);
/* Sticky for this one-operation worker: failed fencing must preserve the VM. */
int remote_mutation_cleanup_pending(void);
#endif
