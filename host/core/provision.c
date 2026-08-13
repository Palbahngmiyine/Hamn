#include "core/provision.h"

#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>

#include "core/log.h"
#include "sshmgr/ssh.h"
#include "util/fs.h"

static int stage_valid(const char *stage)
{
    return stage && (strcmp(stage, "system") == 0 ||
                     strcmp(stage, "user") == 0 ||
                     strcmp(stage, "after-boot") == 0 ||
                     strcmp(stage, "ready") == 0);
}

static int write_hook_log(const struct profile *profile, size_t index,
                          const struct profile_hook *hook, int result)
{
    char logs[PATH_MAX], name[64], path[PATH_MAX], text[512];
    if (!profile_path(profile, "logs", logs, sizeof(logs)) ||
        fs_mkdirs(logs, 0700) != 0)
        return -1;
    int name_length = snprintf(name, sizeof(name), "provision-%s-%zu.log",
                               hook->stage, index);
    if (name_length < 0 || name_length >= (int)sizeof(name) ||
        snprintf(path, sizeof(path), "%s/%s", logs, name) >=
            (int)sizeof(path))
        return -1;
    int length = snprintf(text, sizeof(text),
                          "stage=%s\nindex=%zu\nmode=%s\nresult=%s\n"
                          "exitCode=%d\noutput=redacted\ncommand=redacted\n",
                          hook->stage, index, hook->warn ? "warn" : "fail",
                          result == 0 ? "success" : "failure", result);
    if (length < 0 || length >= (int)sizeof(text)) {
        errno = EOVERFLOW;
        return -1;
    }
    return fs_write_file_atomic(path, text, (size_t)length, 0600);
}

static int run_hook(const struct profile *profile, const char *ip,
                    const struct profile_hook *hook)
{
    char timeout[16];
    int length = snprintf(timeout, sizeof(timeout), "%us",
                          hook->timeout_seconds);
    if (length < 0 || length >= (int)sizeof(timeout)) {
        errno = EOVERFLOW;
        return -1;
    }
    const char *root_command[] = {
        "sudo", "timeout", "--kill-after=5s", timeout, "/bin/bash", "-lc",
        hook->command, NULL,
    };
    const char *user_command[] = {
        "sudo", "-u", "hamn", "--", "timeout", "--kill-after=5s",
        timeout, "/bin/bash", "-lc", hook->command, NULL,
    };
    return ssh_exec(profile, ip,
                    strcmp(hook->stage, "user") == 0 ? user_command :
                                                         root_command,
                    1);
}

int provision_run_stage(const struct profile *profile, const char *ip,
                        const char *stage)
{
    if (!profile || !ip || !ip[0] || !stage_valid(stage)) {
        errno = EINVAL;
        return -1;
    }
    for (size_t index = 0; index < profile->hook_count; index++) {
        const struct profile_hook *hook = &profile->hooks[index];
        if (strcmp(hook->stage, stage) != 0)
            continue;
        int result = run_hook(profile, ip, hook);
        if (write_hook_log(profile, index, hook, result) != 0) {
            logerr("cannot write redacted provision log for %s hook %zu: %s",
                   hook->stage, index, strerror(errno));
            return -1;
        }
        if (result == 0) {
            logmsg("provision hook %zu (%s) completed", index, hook->stage);
            continue;
        }
        logerr("provision hook %zu (%s) failed with exit %d%s", index,
               hook->stage, result, hook->warn ? "; continuing by policy" :
                                                  "");
        if (!hook->warn)
            return -1;
    }
    return 0;
}
