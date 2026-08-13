#include <getopt.h>
#include <stdio.h>
#include <string.h>

#include "cli.h"
#include "core/log.h"
#include "core/profile.h"

static void env_usage(FILE *stream)
{
    fprintf(stream, "usage: hamn env [-p PROFILE] [PROFILE]\n");
}

static int shell_quote(FILE *stream, const char *value)
{
    if (fputc('\'', stream) == EOF)
        return -1;
    for (const char *cursor = value; *cursor; cursor++) {
        if (*cursor == '\'' && fputs("'\\''", stream) == EOF)
            return -1;
        if (*cursor != '\'' && fputc(*cursor, stream) == EOF)
            return -1;
    }
    return fputc('\'', stream) == EOF ? -1 : 0;
}

int cmd_env(int argc, char **argv)
{
    const char *flag_profile = NULL;
    static const struct option options[] = {
        { "profile", required_argument, NULL, 'p' },
        { 0 },
    };
    optind = 1;
    optreset = 1;
    int option;
    while ((option = getopt_long(argc, argv, "p:", options, NULL)) != -1) {
        if (option != 'p' || flag_profile) {
            env_usage(stderr);
            return 2;
        }
        flag_profile = optarg;
    }
    if (optind + 1 < argc) {
        env_usage(stderr);
        return 2;
    }
    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(flag_profile, optind < argc ? argv[optind] : NULL,
                             profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }
    struct profile profile;
    if (profile_load(&profile, profile_name) != 0) {
        logerr("cannot load profile");
        return 1;
    }
    char socket_path[PROFILE_PATH_CAP], docker_host[PROFILE_PATH_CAP + 16];
    if (!profile_path(&profile, "docker.sock", socket_path,
                      sizeof(socket_path)) ||
        snprintf(docker_host, sizeof(docker_host), "unix://%s", socket_path) >=
            (int)sizeof(docker_host)) {
        logerr("cannot resolve Docker socket path");
        return 1;
    }
    if (fputs("export DOCKER_HOST=", stdout) == EOF ||
        shell_quote(stdout, docker_host) != 0 ||
        fputs("\nexport TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE=", stdout) == EOF ||
        shell_quote(stdout, "/var/run/docker.sock") != 0 ||
        fputs("\nexport TESTCONTAINERS_HOST_OVERRIDE=", stdout) == EOF ||
        shell_quote(stdout, "host.docker.internal") != 0 ||
        fputc('\n', stdout) == EOF || fflush(stdout) != 0) {
        logerr("cannot write environment exports");
        return 1;
    }
    return 0;
}
