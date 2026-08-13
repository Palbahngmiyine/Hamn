#ifndef HAMN_PROVISION_H
#define HAMN_PROVISION_H

#include "core/profile.h"

/* Run all hooks for one lifecycle stage. Hook output and commands are never
 * persisted; the per-hook log contains only redacted outcome metadata. */
int provision_run_stage(const struct profile *profile, const char *ip,
                        const char *stage);

#endif
