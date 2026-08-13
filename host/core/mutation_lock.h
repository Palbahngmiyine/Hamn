#ifndef HAMN_MUTATION_LOCK_H
#define HAMN_MUTATION_LOCK_H

#include "core/profile.h"

/* Cross-process, non-blocking lock shared by every default-profile mutation. */
int profile_mutation_lock(const struct profile *profile);
void profile_mutation_unlock(int fd);

#endif
