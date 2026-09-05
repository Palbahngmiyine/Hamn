#ifndef HAMN_RETIREMENT_H
#define HAMN_RETIREMENT_H
#include "core/profile.h"
/* Caller owns lifecycle and profile mutation locks. No action for new profiles. */
int retirement_run(struct profile *profile, const char *ip);
/* Retain an unavailable-context tombstone without changing the source kubeconfig. */
int retirement_context(const struct profile *profile);
#endif
