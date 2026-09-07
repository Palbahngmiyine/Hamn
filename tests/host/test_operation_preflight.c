#include "core/operation.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int proc_cancelled(void) { return 0; }
int proc_start_identity(pid_t pid, uint64_t *sec, uint64_t *usec)
{ (void)pid; *sec = 123; *usec = 456; return 0; }
int proc_executable_identity(pid_t pid, unsigned char uuid[16])
{ (void)pid; memset(uuid, 0, 16); return 0; }
void proc_executable_uuid_format(const unsigned char uuid[16], char hex[33])
{ (void)uuid; memset(hex, '0', 32); hex[32] = 0; }
const char *profile_path(const struct profile *p, const char *file, char *out, size_t cap)
{ return snprintf(out, cap, "%s/%s", p->dir, file) < (int)cap ? out : NULL; }

static void status_is(const struct profile *profile, const char *expected)
{
    cJSON *snapshot = operation_snapshot(profile);
    assert(snapshot);
    assert(!strcmp(cJSON_GetObjectItem(snapshot, "status")->valuestring, expected));
    cJSON_Delete(snapshot);
}

int main(void)
{
    char directory[] = "/tmp/hamn-operation-preflight-XXXXXX", path[1024];
    assert(mkdtemp(directory));
    struct profile profile = {0};
    strcpy(profile.dir, directory);
    assert(operation_begin(&profile, "vm start") == 0);
    assert(operation_finish_unchanged(1) == 1);
    status_is(&profile, "failed");
    assert(operation_begin(&profile, "vm start") == 0);
    assert(operation_finish(1, 0) == 1);
    status_is(&profile, "outcomeUnknown");
    assert(operation_begin(&profile, "vm start") == 0);
    assert(operation_finish_unchanged(1) == 1);
    status_is(&profile, "outcomeUnknown");
    profile_path(&profile, "operation.json", path, sizeof(path));
    assert(unlink(path) == 0 && rmdir(directory) == 0);
    puts("PASS: preflight failure is known; prior interrupted state remains unknown");
}
