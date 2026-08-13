#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "cli.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/profile.h"
#include "core/state.h"
#include "sshmgr/ssh.h"

static void ssh_usage(FILE *stream)
{
    fprintf(stream,
            "usage: hamn ssh [-p PROFILE] [PROFILE --] [-- command...]\n");
}

int cmd_ssh(int argc, char **argv)
{
    const char *flag_profile = NULL;
    const char *positional_profile = NULL;
    int command_index = 1;
    while (command_index < argc) {
        const char *argument = argv[command_index];
        if (strcmp(argument, "--profile") == 0 || strcmp(argument, "-p") == 0) {
            if (++command_index >= argc || flag_profile) {
                ssh_usage(stderr);
                return 2;
            }
            flag_profile = argv[command_index++];
            continue;
        }
        if (strncmp(argument, "--profile=", 10) == 0) {
            if (flag_profile || !argument[10]) {
                ssh_usage(stderr);
                return 2;
            }
            flag_profile = argument + 10;
            command_index++;
            continue;
        }
        if (strcmp(argument, "--") == 0) {
            command_index++;
        } else if (command_index + 1 < argc &&
                   strcmp(argv[command_index + 1], "--") == 0) {
            positional_profile = argument;
            command_index += 2;
        }
        break;
    }

    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(flag_profile, positional_profile, profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }
    struct profile profile;
    if (profile_load(&profile, profile_name) != 0)
        die("cannot load profile");
    if (vm_running_pid(&profile) < 0)
        die("hamn is not running (run: hamn start)");

    struct vm_state state;
    state_load(&profile, &state);
    if (!state.ip[0])
        die("guest IP is unknown; restart with: hamn start");

    const char *args[SSH_ARGV_MAX];
    struct ssh_strbuf strings;
    char destination[128];
    int count = ssh_base_argv(&profile, args, SSH_ARGV_MAX, &strings);
    if (count < 0)
        die("internal: ssh argv overflow");
    snprintf(destination, sizeof(destination), "%s@%s", SSH_USER, state.ip);
    args[count++] = destination;
    for (int index = command_index; index < argc; index++) {
        if (count >= SSH_ARGV_MAX - 1)
            die("too many arguments");
        args[count++] = argv[index];
    }
    args[count] = NULL;
    execvp("ssh", (char *const *)(unsigned long)args);
    die("cannot exec ssh");
}
