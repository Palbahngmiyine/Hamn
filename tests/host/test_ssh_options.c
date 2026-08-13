#include <stdio.h>
#include <string.h>

#include "core/profile.h"
#include "sshmgr/ssh.h"

static int contains(const char *const argv[], int count, const char *value)
{
    for (int index = 0; index < count; index++) {
        if (strcmp(argv[index], value) == 0)
            return 1;
    }
    return 0;
}

static int test_agent_forwarding(void)
{
    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.dir, sizeof(profile.dir), "/tmp/hamn-ssh-options");
    const char *argv[SSH_ARGV_MAX];
    struct ssh_strbuf strings;

    int count = ssh_base_argv(&profile, argv, SSH_ARGV_MAX, &strings);
    if (count < 1 || strcmp(argv[0], "ssh") != 0 ||
        contains(argv, count, "-A")) {
        fprintf(stderr, "SSH agent forwarding was enabled by default\n");
        return -1;
    }

    profile.ssh_agent = 1;
    count = ssh_base_argv(&profile, argv, SSH_ARGV_MAX, &strings);
    if (count < 2 || !contains(argv, count, "-A") ||
        strcmp(argv[count - 1], "-A") != 0) {
        fprintf(stderr, "SSH agent forwarding was not enabled explicitly\n");
        return -1;
    }
    if (ssh_base_argv(&profile, argv, count, &strings) != -1) {
        fprintf(stderr, "SSH option buffer overflow was accepted\n");
        return -1;
    }
    return 0;
}

int main(void)
{
    if (test_agent_forwarding() != 0)
        return 1;
    puts("PASS: SSH agent forwarding stays opt-in and bounded");
    return 0;
}
