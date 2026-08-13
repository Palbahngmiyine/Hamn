#include <errno.h>
#include <getopt.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#include "cli.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"
#include "util/fs.h"
#include "util/proc.h"

struct delete_options {
    int data;
    const char *flag_profile;
    const char *positional_profile;
};

static void delete_usage(FILE *stream)
{
    fprintf(stream, "usage: hamn delete [-p PROFILE] [PROFILE] [--data]\n");
}

static int parse_delete_options(int argc, char **argv,
                                struct delete_options *options)
{
    memset(options, 0, sizeof(*options));
    static const struct option opts[] = {
        { "data", no_argument, NULL, 'D' },
        { "profile", required_argument, NULL, 'p' },
        { 0 },
    };
    optind = 1;
    optreset = 1;
    int option;
    while ((option = getopt_long(argc, argv, "p:", opts, NULL)) != -1) {
        if (option == 'D')
            options->data = 1;
        else if (option == 'p' && !options->flag_profile)
            options->flag_profile = optarg;
        else {
            delete_usage(stderr);
            return -1;
        }
    }
    if (optind + 1 < argc) {
        delete_usage(stderr);
        return -1;
    }
    if (optind < argc)
        options->positional_profile = argv[optind];
    return 0;
}

static int profile_delete_path_safe(const struct profile *profile)
{
    char root[PROFILE_PATH_CAP];
    if (!hamn_home(root, sizeof(root)))
        return 0;
    size_t root_length = strlen(root);
    return profile_name_valid(profile->name) &&
           strncmp(profile->dir, root, root_length) == 0 &&
           profile->dir[root_length] == '/' &&
           strcmp(profile->dir + root_length + 1, profile->name) == 0;
}

static unsigned long long disk_image_bytes(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP];
    struct stat status;
    if (!profile_path(profile, "disk.img", path, sizeof(path)) ||
        stat(path, &status) != 0 || status.st_size < 0)
        return 0;
    return (unsigned long long)status.st_size;
}

static int confirm_data_delete(const struct profile *profile)
{
    fprintf(stderr,
            "this permanently deletes profile %s and all VM, Docker, "
            "containerd, and Kubernetes data in %s\n"
            "disk image currently reserves %llu bytes. Type exactly y to continue: ",
            profile->name, profile->dir, disk_image_bytes(profile));
    char answer[8];
    if (!fgets(answer, sizeof(answer), stdin) || strcmp(answer, "y\n") != 0) {
        logmsg("aborted");
        return 0;
    }
    return 1;
}

static int cmd_delete_locked(const struct delete_options *options,
                             const char *profile_name)
{
    struct profile profile;
    int legacy = 0;
    if (profile_load(&profile, profile_name) != 0) {
        legacy = errno == EPROTONOSUPPORT;
        if (!legacy)
            die("cannot load profile");
    }
    if (legacy && !options->data) {
        logerr("profile %s has a removed runtime setting; use "
               "hamn delete --data --profile %s to remove it", profile_name,
               profile_name);
        return 1;
    }
    int mutation = profile_mutation_lock(&profile);
    if (mutation < 0) {
        logerr("another %s profile mutation is running", profile.name);
        return 1;
    }
    int stop_result = vm_stop(&profile, NULL);
    if (stop_result != VM_STOP_OK) {
        profile_mutation_unlock(mutation);
        die("refusing to delete while VM or host port-forward ownership is "
            "uncertain");
    }

    if (!options->data) {
        char deleted[PROFILE_PATH_CAP];
        if (!profile_path(&profile, "deleted", deleted, sizeof(deleted)) ||
            fs_write_file_atomic(deleted, "soft-deleted\n", 13, 0600) != 0) {
            logerr("cannot mark the soft-deleted profile: %s", strerror(errno));
            profile_mutation_unlock(mutation);
            return 1;
        }
        profile_mutation_unlock(mutation);
        logmsg("deleted VM for profile %s; its disk data is preserved", profile.name);
        return 0;
    }
    if (!profile_delete_path_safe(&profile)) {
        profile_mutation_unlock(mutation);
        die("refusing to delete suspicious path: %s", profile.dir);
    }
    if (!confirm_data_delete(&profile)) {
        profile_mutation_unlock(mutation);
        return 1;
    }
    const char *remove[] = { "rm", "-rf", profile.dir, NULL };
    if (proc_run(remove) != 0) {
        profile_mutation_unlock(mutation);
        die("failed to remove %s", profile.dir);
    }
    profile_mutation_unlock(mutation);
    logmsg("deleted profile %s and all of its data", profile.name);
    return 0;
}

int cmd_delete(int argc, char **argv)
{
    struct delete_options options;
    if (parse_delete_options(argc, argv, &options) != 0)
        return 2;
    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(options.flag_profile, options.positional_profile,
                             profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0) {
        logerr("cannot lock the %s profile lifecycle", profile_name);
        return 1;
    }
    int rc = cmd_delete_locked(&options, profile_name);
    vm_lifecycle_lock_release(&lock);
    return rc;
}
