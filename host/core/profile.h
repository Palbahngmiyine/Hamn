#ifndef HAMN_PROFILE_H
#define HAMN_PROFILE_H

#include <limits.h>
#include <stddef.h>
#include <stdio.h>

#define PROFILE_NAME_CAP 64
#define PROFILE_PATH_CAP 1024
#define PROFILE_MAX_MOUNTS 16
#define PROFILE_MAX_HOOKS 16
#define PROFILE_HOOK_COMMAND_CAP 1024

struct profile_mount {
    char location[PATH_MAX];
    char mount_point[PATH_MAX];
    int writable;
};

struct profile_hook {
    char stage[16];
    char command[PROFILE_HOOK_COMMAND_CAP];
    unsigned timeout_seconds;
    int warn;
};

/* A profile owns one VM and its Docker socket at ~/.hamn/<name>. */
struct profile {
    char name[PROFILE_NAME_CAP];
    char dir[PROFILE_PATH_CAP];
    unsigned cpus;
    unsigned mem_mib;
    unsigned disk_gib;
    int mount_home;
    int home_read_only;
    int mount_inotify;
    char docker_daemon_json[4096];
    int kubernetes_enabled;
    char kubernetes_version[64];
    int rosetta;
    int nested_virtualization;
    int ssh_agent;
    struct profile_mount mounts[PROFILE_MAX_MOUNTS];
    size_t mount_count;
    struct profile_hook hooks[PROFILE_MAX_HOOKS];
    size_t hook_count;
};

int profile_name_valid(const char *name);
int profile_parse_positive(const char *text, unsigned *value);
/* Docker daemon settings are a strict JSON object that cannot replace
 * Hamn-owned Docker/containerd boundaries. */
int profile_docker_daemon_json_valid(const char *text);

/* Resolve --profile > positional profile > HAMN_PROFILE > default. */
int profile_resolve_name(const char *flag_name, const char *positional_name,
                         char out[PROFILE_NAME_CAP]);

/* ~/.hamn path. Successful calls return buf. */
const char *hamn_home(char *buf, size_t cap);

/* Create the profile directory (0700), then load config.yaml or defaults. */
int profile_load(struct profile *profile, const char *name);

/* Save config.yaml atomically. Legacy hamn.conf configurations are never
 * converted in place and return EPROTONOSUPPORT. */
int profile_save(const struct profile *profile);

/* Emit a copyable default YAML template. */
int profile_template_print(FILE *out);

/* Docker context is hamn for default and hamn-<profile> otherwise. */
int profile_docker_context_name(const struct profile *profile, char *out,
                                size_t cap);

/* p->dir/<file> path. Successful calls return buf. */
const char *profile_path(const struct profile *profile, const char *file,
                         char *buf, size_t cap);

#endif
