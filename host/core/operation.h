#ifndef HAMN_OPERATION_H
#define HAMN_OPERATION_H
#include "core/profile.h"
#include "cjson/cJSON.h"
int operation_begin(const struct profile *profile, const char *name);
int operation_phase(const char *phase);
int operation_finish(int result, int restored);
int operation_finish_unchanged(int result);
void operation_started_vm(void);
cJSON *operation_snapshot(const struct profile *profile);
#endif
