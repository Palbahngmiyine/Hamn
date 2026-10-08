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
    int rosetta;
    int nested_virtualization;
    int ssh_agent;
    struct profile_mount mounts[PROFILE_MAX_MOUNTS];
    size_t mount_count;
    struct profile_hook hooks[PROFILE_MAX_HOOKS];
    size_t hook_count;
};

/* The settings config.yaml stores, in the order it lists them. */
enum profile_setting {
    PROFILE_SETTING_CPUS,
    PROFILE_SETTING_MEMORY_MIB,
    PROFILE_SETTING_DISK_GIB,
    PROFILE_SETTING_MOUNT_HOME,
    PROFILE_SETTING_HOME_READ_ONLY,
    PROFILE_SETTING_MOUNT_INOTIFY,
    PROFILE_SETTING_DOCKER_DAEMON_JSON,
    PROFILE_SETTING_ROSETTA,
    PROFILE_SETTING_NESTED_VIRTUALIZATION,
    PROFILE_SETTING_SSH_AGENT,
    PROFILE_SETTING_MOUNTS,
    PROFILE_SETTING_PROVISION,
    PROFILE_SETTING_COUNT
};

/* Capacity, with the terminating NUL, of the reason for a refused profile. */
#define PROFILE_REASON_CAP 256

int profile_name_valid(const char *name);
int profile_parse_positive(const char *text, unsigned *value);
/* Docker daemon settings are a strict JSON object that cannot replace
 * Hamn-owned Docker/containerd boundaries. */
int profile_docker_daemon_json_valid(const char *text);

/* ~/.hamn path. Successful calls return buf. */
const char *hamn_home(char *buf, size_t cap);

/* Create the profile directory (0700), then load config.yaml or defaults. */
int profile_load(struct profile *profile, const char *name);

/* Read an existing profile without creating directories or a configuration. */
int profile_read_existing(struct profile *profile, const char *name);

/* profile_read_existing that also says why it failed. On -1 errno is as for
 * profile_read_existing (ENOENT: the profile has no config.yaml) and reason
 * holds a NUL-terminated explanation: the rule config.yaml breaks, or the
 * error text. A reason can quote the file, so it holds printable ASCII only;
 * any other byte is replaced with '?'. On 0 reason is empty. */
int profile_read_existing_reason(struct profile *profile, const char *name,
                                 char reason[PROFILE_REASON_CAP]);

/* The config.yaml key of a setting, such as "memoryMiB" or
 * "docker.daemonJson"; NULL for a value that is not a setting. */
const char *profile_setting_key(enum profile_setting setting);

/* The settings in which two profiles differ: bit (1u << setting) is set for
 * each, so 0 means equal settings. name and dir are not settings. Booleans
 * compare as true or false. Mounts and provisioning hooks compare entry by
 * entry in order: the position of a mount is its share tag and the position
 * of a hook is its place in the run order. Strings compare byte for byte. */
unsigned profile_diff(const struct profile *a, const struct profile *b);

/* Save config.yaml atomically. */
int profile_save(const struct profile *profile);

/* p->dir/<file> path. Successful calls return buf. */
const char *profile_path(const struct profile *profile, const char *file,
                         char *buf, size_t cap);

#endif
