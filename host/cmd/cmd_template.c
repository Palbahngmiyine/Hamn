#include <getopt.h>
#include <stdio.h>

#include "cli.h"
#include "core/log.h"
#include "core/profile.h"

static void template_usage(FILE *stream)
{
    fprintf(stream, "usage: hamn template [-p PROFILE] [PROFILE]\n");
}

int cmd_template(int argc, char **argv)
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
            template_usage(stderr);
            return 2;
        }
        flag_profile = optarg;
    }
    if (optind + 1 < argc) {
        template_usage(stderr);
        return 2;
    }
    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(flag_profile, optind < argc ? argv[optind] : NULL,
                             profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }
    (void)profile_name;
    if (profile_template_print(stdout) != 0) {
        logerr("cannot write the profile template");
        return 1;
    }
    return 0;
}
