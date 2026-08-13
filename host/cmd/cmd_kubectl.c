#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "cli.h"
#include "core/log.h"
#include "core/profile.h"

static void kubectl_usage(FILE *stream)
{
    fprintf(stream,
            "usage: hamn kubectl [-p PROFILE] [PROFILE --] <kubectl args...>\n");
}

static int kubectl_argument_overrides_config(const char *argument)
{
    return strcmp(argument, "--kubeconfig") == 0 ||
           strncmp(argument, "--kubeconfig=", 13) == 0;
}

int cmd_kubectl(int argc, char **argv)
{
    const char *flag_profile = NULL;
    const char *positional_profile = NULL;
    int command_start = 1;
    while (command_start < argc) {
        const char *argument = argv[command_start];
        if (strcmp(argument, "--profile") == 0 ||
            strcmp(argument, "-p") == 0) {
            if (++command_start >= argc || flag_profile) {
                kubectl_usage(stderr);
                return 2;
            }
            flag_profile = argv[command_start++];
            continue;
        }
        if (strncmp(argument, "--profile=", 10) == 0) {
            if (flag_profile || !argument[10]) {
                kubectl_usage(stderr);
                return 2;
            }
            flag_profile = argument + 10;
            command_start++;
            continue;
        }
        if (command_start + 1 < argc &&
            strcmp(argv[command_start + 1], "--") == 0) {
            positional_profile = argument;
            command_start += 2;
        }
        break;
    }
    if (command_start >= argc) {
        kubectl_usage(stderr);
        return 2;
    }

    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(flag_profile, positional_profile, profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }
    struct profile profile;
    if (profile_load(&profile, profile_name) != 0) {
        logerr("cannot load profile");
        return 1;
    }
    if (!profile.kubernetes_enabled) {
        logerr("Kubernetes is disabled for profile %s; run: hamn kubernetes "
               "start --profile %s", profile.name, profile.name);
        return 1;
    }

    char kubeconfig[PROFILE_PATH_CAP];
    if (!profile_path(&profile, "kubeconfig", kubeconfig, sizeof(kubeconfig)) ||
        access(kubeconfig, R_OK) != 0) {
        logerr("kubeconfig is not ready; run: hamn kubernetes start");
        return 1;
    }
    for (int index = command_start; index < argc; index++) {
        if (kubectl_argument_overrides_config(argv[index])) {
            logerr("hamn kubectl only operates on the current profile");
            return 2;
        }
    }

    const int command_count = argc - command_start;
    char *child_argv[command_count + 4];
    child_argv[0] = "kubectl";
    child_argv[1] = "--kubeconfig";
    child_argv[2] = kubeconfig;
    for (int index = 0; index < command_count; index++)
        child_argv[index + 3] = argv[command_start + index];
    child_argv[command_count + 3] = NULL;
    execvp(child_argv[0], child_argv);
    logerr("cannot execute kubectl; install it separately: %s", strerror(errno));
    return 1;
}
