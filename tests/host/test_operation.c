#include "core/operation.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <poll.h>
#include <signal.h>
#include <sys/wait.h>
static int alive = 1, cancelled;
int proc_cancelled(void) { return cancelled; }
int proc_start_identity(pid_t pid, uint64_t *sec, uint64_t *usec)
{ (void)pid; *sec = 123; *usec = 456; return alive ? 0 : -1; }
int proc_executable_identity(pid_t pid, unsigned char uuid[16])
{ (void)pid; memset(uuid, 0, 16); return 0; }
void proc_executable_uuid_format(const unsigned char uuid[16], char hex[33])
{ (void)uuid; memset(hex, '0', 32); hex[32] = 0; }
const char *profile_path(const struct profile *p, const char *file, char *out, size_t cap)
{ return snprintf(out, cap, "%s/%s", p->dir, file) < (int)cap ? out : NULL; }
int main(void)
{
    char directory[] = "/tmp/hamn-operation-XXXXXX", path[1024];
    assert(mkdtemp(directory)); struct profile p = {0}; strcpy(p.dir, directory);
    cJSON *value = operation_snapshot(&p); assert(cJSON_IsNull(value)); cJSON_Delete(value);
    assert(operation_begin(&p, "vm start") == 0);
    assert(operation_phase("forwarding") == 0); operation_started_vm();
    alive = 0; value = operation_snapshot(&p);
    assert(!strcmp(cJSON_GetObjectItem(value, "status")->valuestring, "outcomeUnknown"));
    assert(cJSON_IsTrue(cJSON_GetObjectItem(value, "startedVm"))); cJSON_Delete(value);
    assert(operation_finish(0, 1) == 0);
    value = operation_snapshot(&p);
    assert(!strcmp(cJSON_GetObjectItem(value, "status")->valuestring, "completed")); cJSON_Delete(value);
    alive = 1;
    assert(operation_begin(&p, "vm stop") == 0);
    cancelled = 1;
    assert(operation_finish(1, 1) == 130);
    value = operation_snapshot(&p);
    assert(!strcmp(cJSON_GetObjectItem(value, "status")->valuestring, "cancelled")); cJSON_Delete(value);
    assert(operation_begin(&p, "vm start") == 0);
    assert(operation_finish(1, 0) == 1);
    value = operation_snapshot(&p);
    assert(!strcmp(cJSON_GetObjectItem(value, "status")->valuestring, "outcomeUnknown")); cJSON_Delete(value);
    profile_path(&p, "operation.json", path, sizeof(path)); assert(chmod(path, 0644) == 0);
    assert(!operation_snapshot(&p)); assert(unlink(path) == 0);
    assert(mkfifo(path, 0600) == 0);
    int gate[2]; assert(pipe(gate) == 0);
    pid_t child = fork(); assert(child >= 0);
    if (child == 0) {
        close(gate[0]);
        assert(!operation_snapshot(&p));
        assert(operation_begin(&p, "vm start") == -1);
        _exit(0);
    }
    close(gate[1]);
    struct pollfd ready = { .fd = gate[0], .events = POLLIN };
    int completed = poll(&ready, 1, 5000);
    if (completed <= 0) kill(child, SIGKILL);
    int status; assert(waitpid(child, &status, 0) == child);
    close(gate[0]);
    assert(unlink(path) == 0); assert(rmdir(directory) == 0);
    assert(completed > 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    puts("PASS: operation ownership, completion and unsafe record handling");
}
