#include <assert.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "core/profile.h"
#include "core/provision.h"

static int calls;
static char last_command[PROFILE_HOOK_COMMAND_CAP];
static char last_user[32];
static char last_timeout[16];

const char *profile_path(const struct profile *profile, const char *file,
                         char *output, size_t capacity)
{
    int length = snprintf(output, capacity, "%s/%s", profile->dir, file);
    return length >= 0 && (size_t)length < capacity ? output : NULL;
}

int ssh_exec(const struct profile *profile, const char *ip,
             const char *const remote_argv[], int quiet)
{
    (void)profile;
    (void)ip;
    (void)quiet;
    calls++;
    if (strcmp(remote_argv[0], "sudo") != 0)
        return -1;
    snprintf(last_user, sizeof(last_user), "%s",
             strcmp(remote_argv[1], "-u") == 0 ? remote_argv[2] : "root");
    size_t index = 0;
    while (remote_argv[index])
        index++;
    for (size_t item = 0; item + 2 < index; item++) {
        if (strcmp(remote_argv[item], "timeout") == 0) {
            assert(strcmp(remote_argv[item + 1], "--kill-after=5s") == 0);
            snprintf(last_timeout, sizeof(last_timeout), "%s",
                     remote_argv[item + 2]);
            break;
        }
    }
    snprintf(last_command, sizeof(last_command), "%s", remote_argv[index - 1]);
    return strcmp(last_command, "fail") == 0 ? 7 : 0;
}

void logmsg(const char *format, ...)
{
    (void)format;
}

void logerr(const char *format, ...)
{
    (void)format;
}

static void read_text(const char *path, char *output, size_t capacity)
{
    FILE *file = fopen(path, "r");
    assert(file);
    size_t count = fread(output, 1, capacity - 1, file);
    assert(!ferror(file));
    assert(fclose(file) == 0);
    output[count] = '\0';
}

int main(void)
{
    char root[] = "/tmp/hamn-provision.XXXXXX";
    assert(mkdtemp(root));
    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.name, sizeof(profile.name), "default");
    snprintf(profile.dir, sizeof(profile.dir), "%s", root);
    profile.hook_count = 3;
    snprintf(profile.hooks[0].stage, sizeof(profile.hooks[0].stage), "system");
    snprintf(profile.hooks[0].command, sizeof(profile.hooks[0].command),
             "echo token=super-secret");
    profile.hooks[0].timeout_seconds = 17;
    snprintf(profile.hooks[1].stage, sizeof(profile.hooks[1].stage), "user");
    snprintf(profile.hooks[1].command, sizeof(profile.hooks[1].command),
             "fail");
    profile.hooks[1].timeout_seconds = 3;
    profile.hooks[1].warn = 1;
    snprintf(profile.hooks[2].stage, sizeof(profile.hooks[2].stage),
             "after-boot");
    snprintf(profile.hooks[2].command, sizeof(profile.hooks[2].command),
             "fail");
    profile.hooks[2].timeout_seconds = 1;

    assert(provision_run_stage(&profile, "192.0.2.2", "system") == 0);
    assert(calls == 1 && strcmp(last_user, "root") == 0);
    assert(strcmp(last_command, "echo token=super-secret") == 0);
    assert(strcmp(last_timeout, "17s") == 0);
    assert(provision_run_stage(&profile, "192.0.2.2", "user") == 0);
    assert(calls == 2 && strcmp(last_user, "hamn") == 0);
    assert(strcmp(last_timeout, "3s") == 0);
    assert(provision_run_stage(&profile, "192.0.2.2", "after-boot") == -1);
    assert(calls == 3);
    assert(strcmp(last_timeout, "1s") == 0);

    char path[1024], text[1024];
    snprintf(path, sizeof(path), "%s/logs/provision-system-0.log", root);
    read_text(path, text, sizeof(text));
    assert(strstr(text, "result=success") && strstr(text, "output=redacted") &&
           strstr(text, "command=redacted") && !strstr(text, "super-secret"));
    snprintf(path, sizeof(path), "%s/logs/provision-user-1.log", root);
    read_text(path, text, sizeof(text));
    assert(strstr(text, "mode=warn") && strstr(text, "result=failure") &&
           strstr(text, "exitCode=7"));
    snprintf(path, sizeof(path), "%s/logs/provision-after-boot-2.log", root);
    read_text(path, text, sizeof(text));
    assert(strstr(text, "mode=fail") && strstr(text, "result=failure"));

    assert(unlink(path) == 0);
    snprintf(path, sizeof(path), "%s/logs/provision-user-1.log", root);
    assert(unlink(path) == 0);
    snprintf(path, sizeof(path), "%s/logs/provision-system-0.log", root);
    assert(unlink(path) == 0);
    snprintf(path, sizeof(path), "%s/logs", root);
    assert(rmdir(path) == 0 && rmdir(root) == 0);
    puts("PASS: provision hooks time out remotely and redact logs");
    return 0;
}
