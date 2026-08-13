#include <getopt.h>
#include <stdio.h>

#include "cli.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"

static int resolve_stop_profile(int argc, char **argv,
                                char profile_name[PROFILE_NAME_CAP])
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
            fprintf(stderr, "usage: hamn stop [-p PROFILE] [PROFILE]\n");
            return -1;
        }
        flag_profile = optarg;
    }
    if (optind + 1 < argc ||
        profile_resolve_name(flag_profile, optind < argc ? argv[optind] : NULL,
                             profile_name) != 0) {
        fprintf(stderr, "usage: hamn stop [-p PROFILE] [PROFILE]\n");
        return -1;
    }
    return 0;
}

static int cmd_stop_locked(const char *profile_name)
{
    struct profile profile;
    if (profile_load(&profile, profile_name) != 0)
        die("cannot load profile");
    int mutation_fd = profile_mutation_lock(&profile);
    if (mutation_fd < 0) {
        logerr("another %s profile mutation is running", profile.name);
        return 1;
    }

    int was_running = 0;
    if (vm_stop(&profile, &was_running) != 0) {
        profile_mutation_unlock(mutation_fd);
        return 1;
    }
    profile_mutation_unlock(mutation_fd);
    logmsg(was_running ? "stopped" : "not running");
    return 0;
}

int cmd_stop(int argc, char **argv)
{
    char profile_name[PROFILE_NAME_CAP];
    if (resolve_stop_profile(argc, argv, profile_name) != 0)
        return 2;
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0) {
        logerr("cannot lock the %s profile lifecycle", profile_name);
        return 1;
    }
    int rc = cmd_stop_locked(profile_name);
    vm_lifecycle_lock_release(&lock);
    return rc;
}
