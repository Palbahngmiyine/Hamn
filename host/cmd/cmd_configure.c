#include <errno.h>
#include <getopt.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cjson/cJSON.h"
#include "cli.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"

struct configure_options {
    unsigned cpus;
    unsigned memory_gib;
    unsigned disk_gib;
    int cpus_set;
    int memory_set;
    int disk_set;
    int mount_home;
    int mount_home_set;
    int home_read_only;
    int home_read_only_set;
    int mount_inotify;
    int mount_inotify_set;
    const char *docker_daemon_json;
    int kubernetes_enabled;
    int kubernetes_enabled_set;
    int rosetta;
    int rosetta_set;
    int nested_virtualization;
    int nested_virtualization_set;
    int ssh_agent;
    int ssh_agent_set;
    struct profile_mount mounts[PROFILE_MAX_MOUNTS];
    size_t mount_count;
    int clear_mounts;
    struct profile_hook hooks[PROFILE_MAX_HOOKS];
    size_t hook_count;
    int clear_hooks;
    int json_output;
    const char *flag_profile;
    const char *positional_profile;
};

static int parse_bool(const char *text, int *value)
{
    if (strcmp(text, "true") == 0) {
        *value = 1;
        return 0;
    }
    if (strcmp(text, "false") == 0) {
        *value = 0;
        return 0;
    }
    return -1;
}

static int parse_bool_once(const char *text, int *set, int *value)
{
    if (*set || parse_bool(text, value) != 0)
        return -1;
    *set = 1;
    return 0;
}

static int parse_mount(const char *text, struct profile_mount *mount)
{
    char copy[PATH_MAX * 2 + 16];
    if (!text || strlen(text) >= sizeof(copy))
        return -1;
    snprintf(copy, sizeof(copy), "%s", text);
    char *first = strchr(copy, ':');
    char *last = strrchr(copy, ':');
    if (!first || first == last)
        return -1;
    *first = '\0';
    *last = '\0';
    const char *mode = last + 1;
    if (!copy[0] || !first[1] ||
        snprintf(mount->location, sizeof(mount->location), "%s", copy) >=
            (int)sizeof(mount->location) ||
        snprintf(mount->mount_point, sizeof(mount->mount_point), "%s",
                 first + 1) >= (int)sizeof(mount->mount_point))
        return -1;
    if (strcmp(mode, "rw") == 0)
        mount->writable = 1;
    else if (strcmp(mode, "ro") == 0)
        mount->writable = 0;
    else
        return -1;
    return 0;
}

static int parse_hook(const char *text, struct profile_hook *hook)
{
    char copy[PROFILE_HOOK_COMMAND_CAP + 96];
    if (!text || strlen(text) >= sizeof(copy))
        return -1;
    snprintf(copy, sizeof(copy), "%s", text);
    char *stage = copy;
    char *first = strchr(stage, ':');
    if (!first)
        return -1;
    *first++ = '\0';
    char *timeout = first;
    char *second = strchr(timeout, ':');
    if (!second)
        return -1;
    *second++ = '\0';
    char *mode = second;
    char *third = strchr(mode, ':');
    if (!third)
        return -1;
    *third++ = '\0';
    if (!stage[0] || !timeout[0] || !mode[0] || !third[0] ||
        profile_parse_positive(timeout, &hook->timeout_seconds) != 0 ||
        snprintf(hook->stage, sizeof(hook->stage), "%s", stage) >=
            (int)sizeof(hook->stage) ||
        snprintf(hook->command, sizeof(hook->command), "%s", third) >=
            (int)sizeof(hook->command))
        return -1;
    if (strcmp(mode, "fail") == 0)
        hook->warn = 0;
    else if (strcmp(mode, "warn") == 0)
        hook->warn = 1;
    else
        return -1;
    return 0;
}

static int print_json(const struct profile *profile)
{
    cJSON *root = cJSON_CreateObject();
    if (!root || !cJSON_AddNumberToObject(root, "schemaVersion", 1) ||
        !cJSON_AddStringToObject(root, "profile", profile->name) ||
        !cJSON_AddNumberToObject(root, "cpus", profile->cpus) ||
        !cJSON_AddNumberToObject(root, "memoryMiB", profile->mem_mib) ||
        !cJSON_AddNumberToObject(root, "diskGiB", profile->disk_gib) ||
        !cJSON_AddBoolToObject(root, "mountHome", profile->mount_home) ||
        !cJSON_AddBoolToObject(root, "mountInotify", profile->mount_inotify) ||
        !cJSON_AddBoolToObject(root, "kubernetesEnabled",
                               profile->kubernetes_enabled)) {
        cJSON_Delete(root);
        return -1;
    }
    char *text = cJSON_PrintUnformatted(root);
    cJSON_Delete(root);
    if (!text)
        return -1;
    printf("%s\n", text);
    cJSON_free(text);
    return 0;
}

static void configure_usage(FILE *stream)
{
    fprintf(stream,
            "usage: hamn configure [-p PROFILE] [PROFILE] OPTIONS\n"
            "  --cpu N --memory GiB --disk GiB\n"
            "  --mount-home true|false --home-read-only true|false\n"
            "  --mount-inotify true|false\n"
            "  --docker-daemon-json JSON --kubernetes true|false\n"
            "  --rosetta true|false --nested-virtualization true|false\n"
            "  --ssh-agent true|false\n"
            "  --clear-mounts [--mount HOST_PATH:GUEST_PATH:ro|rw]\n"
            "  --clear-provision [--provision-hook STAGE:SECONDS:fail|warn:COMMAND]\n"
            "  [--output json]\n");
}

static int parse_options(int argc, char **argv, struct configure_options *out)
{
    memset(out, 0, sizeof(*out));
    enum {
        OPT_MOUNT_HOME = 1000,
        OPT_HOME_READ_ONLY,
        OPT_MOUNT_INOTIFY,
        OPT_DOCKER_DAEMON_JSON,
        OPT_KUBERNETES,
        OPT_ROSETTA,
        OPT_NESTED_VIRTUALIZATION,
        OPT_SSH_AGENT,
        OPT_MOUNT,
        OPT_CLEAR_MOUNTS,
        OPT_PROVISION_HOOK,
        OPT_CLEAR_PROVISION,
    };
    static const struct option options[] = {
        { "cpu", required_argument, NULL, 'c' },
        { "memory", required_argument, NULL, 'm' },
        { "disk", required_argument, NULL, 'd' },
        { "output", required_argument, NULL, 'o' },
        { "profile", required_argument, NULL, 'p' },
        { "mount-home", required_argument, NULL, OPT_MOUNT_HOME },
        { "home-read-only", required_argument, NULL, OPT_HOME_READ_ONLY },
        { "mount-inotify", required_argument, NULL, OPT_MOUNT_INOTIFY },
        { "docker-daemon-json", required_argument, NULL, OPT_DOCKER_DAEMON_JSON },
        { "kubernetes", required_argument, NULL, OPT_KUBERNETES },
        { "rosetta", required_argument, NULL, OPT_ROSETTA },
        { "nested-virtualization", required_argument, NULL, OPT_NESTED_VIRTUALIZATION },
        { "ssh-agent", required_argument, NULL, OPT_SSH_AGENT },
        { "mount", required_argument, NULL, OPT_MOUNT },
        { "clear-mounts", no_argument, NULL, OPT_CLEAR_MOUNTS },
        { "provision-hook", required_argument, NULL, OPT_PROVISION_HOOK },
        { "clear-provision", no_argument, NULL, OPT_CLEAR_PROVISION },
        { 0 },
    };
    optind = 1;
    optreset = 1;
    int option;
    while ((option = getopt_long(argc, argv, "c:m:d:o:p:", options, NULL)) != -1) {
        switch (option) {
        case 'c':
            if (out->cpus_set || profile_parse_positive(optarg, &out->cpus) != 0)
                return -1;
            out->cpus_set = 1;
            break;
        case 'm':
            if (out->memory_set ||
                profile_parse_positive(optarg, &out->memory_gib) != 0 ||
                out->memory_gib > UINT_MAX / 1024U)
                return -1;
            out->memory_set = 1;
            break;
        case 'd':
            if (out->disk_set || profile_parse_positive(optarg, &out->disk_gib) != 0)
                return -1;
            out->disk_set = 1;
            break;
        case 'o':
            if (out->json_output || strcmp(optarg, "json") != 0)
                return -1;
            out->json_output = 1;
            break;
        case 'p':
            if (out->flag_profile)
                return -1;
            out->flag_profile = optarg;
            break;
        case OPT_MOUNT_HOME:
            if (parse_bool_once(optarg, &out->mount_home_set,
                                &out->mount_home) != 0)
                return -1;
            break;
        case OPT_HOME_READ_ONLY:
            if (parse_bool_once(optarg, &out->home_read_only_set,
                                &out->home_read_only) != 0)
                return -1;
            break;
        case OPT_MOUNT_INOTIFY:
            if (parse_bool_once(optarg, &out->mount_inotify_set,
                                &out->mount_inotify) != 0)
                return -1;
            break;
        case OPT_DOCKER_DAEMON_JSON:
            if (out->docker_daemon_json ||
                !profile_docker_daemon_json_valid(optarg))
                return -1;
            out->docker_daemon_json = optarg;
            break;
        case OPT_KUBERNETES:
            if (parse_bool_once(optarg, &out->kubernetes_enabled_set,
                                &out->kubernetes_enabled) != 0)
                return -1;
            break;
        case OPT_ROSETTA:
            if (parse_bool_once(optarg, &out->rosetta_set, &out->rosetta) != 0)
                return -1;
            break;
        case OPT_NESTED_VIRTUALIZATION:
            if (parse_bool_once(optarg, &out->nested_virtualization_set,
                                &out->nested_virtualization) != 0)
                return -1;
            break;
        case OPT_SSH_AGENT:
            if (parse_bool_once(optarg, &out->ssh_agent_set,
                                &out->ssh_agent) != 0)
                return -1;
            break;
        case OPT_MOUNT:
            if (out->mount_count == PROFILE_MAX_MOUNTS ||
                parse_mount(optarg, &out->mounts[out->mount_count]) != 0)
                return -1;
            out->mount_count++;
            break;
        case OPT_CLEAR_MOUNTS:
            if (out->clear_mounts)
                return -1;
            out->clear_mounts = 1;
            break;
        case OPT_PROVISION_HOOK:
            if (out->hook_count == PROFILE_MAX_HOOKS ||
                parse_hook(optarg, &out->hooks[out->hook_count]) != 0)
                return -1;
            out->hook_count++;
            break;
        case OPT_CLEAR_PROVISION:
            if (out->clear_hooks)
                return -1;
            out->clear_hooks = 1;
            break;
        default:
            return -1;
        }
    }
    if (optind + 1 < argc)
        return -1;
    if (optind < argc)
        out->positional_profile = argv[optind];
    return out->cpus_set || out->memory_set || out->disk_set ||
           out->mount_home_set || out->home_read_only_set ||
           out->mount_inotify_set ||
           out->docker_daemon_json || out->kubernetes_enabled_set ||
           out->rosetta_set ||
           out->nested_virtualization_set || out->ssh_agent_set ||
           out->mount_count || out->clear_mounts || out->hook_count ||
           out->clear_hooks ? 0 : -1;
}

static int apply_options(struct profile *profile,
                         const struct configure_options *options)
{
    if (options->disk_set && options->disk_gib < profile->disk_gib) {
        logerr("disk size cannot shrink (current: %u GiB)", profile->disk_gib);
        return -1;
    }
    if (options->cpus_set)
        profile->cpus = options->cpus;
    if (options->memory_set)
        profile->mem_mib = options->memory_gib * 1024U;
    if (options->disk_set)
        profile->disk_gib = options->disk_gib;
    if (options->mount_home_set)
        profile->mount_home = options->mount_home;
    if (options->home_read_only_set)
        profile->home_read_only = options->home_read_only;
    if (options->mount_inotify_set)
        profile->mount_inotify = options->mount_inotify;
    if (options->docker_daemon_json)
        snprintf(profile->docker_daemon_json, sizeof(profile->docker_daemon_json),
                 "%s", options->docker_daemon_json);
    if (options->kubernetes_enabled_set)
        profile->kubernetes_enabled = options->kubernetes_enabled;
    if (options->rosetta_set)
        profile->rosetta = options->rosetta;
    if (options->nested_virtualization_set)
        profile->nested_virtualization = options->nested_virtualization;
    if (options->ssh_agent_set)
        profile->ssh_agent = options->ssh_agent;
    if (options->clear_mounts)
        profile->mount_count = 0;
    if (profile->mount_count + options->mount_count > PROFILE_MAX_MOUNTS)
        return -1;
    for (size_t index = 0; index < options->mount_count; index++)
        profile->mounts[profile->mount_count++] = options->mounts[index];
    if (options->clear_hooks)
        profile->hook_count = 0;
    if (profile->hook_count + options->hook_count > PROFILE_MAX_HOOKS)
        return -1;
    for (size_t index = 0; index < options->hook_count; index++)
        profile->hooks[profile->hook_count++] = options->hooks[index];
    return 0;
}

int cmd_configure(int argc, char **argv)
{
    struct configure_options options;
    if (parse_options(argc, argv, &options) != 0) {
        configure_usage(stderr);
        return 2;
    }
    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(options.flag_profile, options.positional_profile,
                             profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }

    struct profile profile;
    if (profile_load(&profile, profile_name) != 0) {
        logerr("cannot load profile");
        return 1;
    }
    int mutation_fd = profile_mutation_lock(&profile);
    if (mutation_fd < 0) {
        logerr("another %s profile mutation is running", profile.name);
        return 1;
    }
    int rc = 1;
    if (vm_running_pid(&profile) > 0) {
        logerr("VM settings can only change while the VM is stopped");
        goto out;
    }
    if (apply_options(&profile, &options) != 0) {
        if (errno)
            logerr("invalid profile configuration: %s", strerror(errno));
        goto out;
    }
    if (profile_save(&profile) != 0) {
        logerr("cannot save profile settings: %s", strerror(errno));
        goto out;
    }
    if (options.json_output && print_json(&profile) != 0) {
        logerr("cannot encode profile settings");
        goto out;
    }
    if (!options.json_output) {
        printf("profile %s: cpu=%u memory=%u MiB disk=%u GiB network=shared-nat\n",
               profile.name, profile.cpus, profile.mem_mib, profile.disk_gib);
    }
    rc = 0;
out:
    profile_mutation_unlock(mutation_fd);
    return rc;
}
