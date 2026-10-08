#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
#include "core/profile.h"
#include "core/control.h"
#include "util/fs.h"
#include <string.h>
#include "cjson/cJSON.h"

static int printable(const char *text)
{
    for (const unsigned char *cursor = (const unsigned char *)text; *cursor;
         cursor++) {
        if (*cursor < 0x20 || *cursor > 0x7e)
            return 0;
    }
    return 1;
}

/* A refused config.yaml says which rule it breaks. The profile `reasons` is
 * removed again. */
static void refused_configurations_say_why(const char *root)
{
    static const struct {
        const char *text;
        const char *reason;
    } cases[] = {
        { "cpus: 4\nunknown: true\n", "unknown configuration key: unknown" },
        { "cpus: 4\ncpus: 5\n", "duplicate or invalid configuration key" },
        { "cpus: &cpu 4\n", "YAML anchors and tags are not supported" },
        { "cpus: *cpu\n", "YAML aliases are not supported" },
        { "cpus: !!int 4\n", "YAML anchors and tags are not supported" },
        { "%TAG ! tag:example.com,2000:\n---\ncpus: 4\n",
          "YAML tag directives are not supported" },
        { "", "expected exactly one YAML configuration document" },
        { "cpus: 4\n---\ncpus: 5\n",
          "expected exactly one YAML configuration document" },
        { "- cpus\n", "expected a mapping" },
        { "mountHome: \"true\"\n", "invalid scalar value" },
        { "mountHome: yes\n", "expected boolean true or false" },
        { "cpus: 0\n", "expected a positive integer" },
        { "mounts: true\n", "expected a sequence" },
        { "docker: []\n", "expected a mapping" },
        { "docker:\n  other: 1\n", "unknown docker key: other" },
        { "docker:\n  daemonJson: \"[\"\n",
          "docker.daemonJson must be one JSON object that leaves the "
          "settings Hamn manages alone" },
        { "homeReadOnly: true\nmountHome: false\n",
          "homeReadOnly requires mountHome" },
        { "mountHome: false\nmountInotify: true\n",
          "mountInotify requires a writable share" },
        { "mounts:\n  - location: /a\n",
          "each mount requires location and mountPoint" },
        { "mounts:\n  - location: relative\n    mountPoint: /w\n",
          "a mount location must be a normalized absolute path" },
        { "mounts:\n  - location: /a\n    mountPoint: /\n",
          "a mountPoint must be a normalized absolute path other than /" },
        { "mounts:\n  - location: /a\n    mountPoint: /w\n"
          "  - location: /b\n    mountPoint: /w\n",
          "each mountPoint must be unique" },
        { "provision:\n  - stage: system\n",
          "each provision hook requires command" },
        { "provision:\n  - command: \"\"\n",
          "a provision command must not be empty" },
        { "provision:\n  - command: x\n    stage: invalid\n",
          "a provision stage must be system, user, after-boot or ready" },
        { "provision:\n  - command: x\n    timeoutSeconds: 3601\n",
          "a provision timeoutSeconds must be 1 to 3600" },
        { "provision:\n  - command: x\n    mode: maybe\n",
          "provision mode must be fail or warn" },
        /* A mode that is no scalar: refused before the mode is compared. */
        { "provision:\n  - command: x\n    mode: [warn]\n",
          "expected a scalar value" },
        /* Text of the file is quoted with printable ASCII only: an escape
         * character and each byte of a non-ASCII key become '?'. */
        { "\"\\e[2Jtitle\": 1\n", "unknown configuration key: ?[2Jtitle" },
        { "\xed\x82\xa4: 1\n", "unknown configuration key: ???" },
    };
    char directory[1024], path[1100], reason[PROFILE_REASON_CAP];
    struct profile profile;
    snprintf(directory, sizeof(directory), "%s/reasons", root);
    snprintf(path, sizeof(path), "%s/config.yaml", directory);
    assert(mkdir(directory, 0700) == 0);
    memset(reason, 'x', sizeof(reason));
    assert(profile_read_existing_reason(&profile, "reasons", reason) == -1);
    assert(errno == ENOENT && strcmp(reason, strerror(ENOENT)) == 0);
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        assert(fs_write_file_atomic(path, cases[index].text,
                                    strlen(cases[index].text), 0600) == 0);
        errno = 0;
        int rc = profile_read_existing_reason(&profile, "reasons", reason);
        if (rc != -1 || errno != EINVAL ||
            strcmp(reason, cases[index].reason) != 0) {
            fprintf(stderr, "case %zu: rc %d errno %d reason \"%s\"\n", index,
                    rc, errno, reason);
            abort();
        }
        /* The parser that gives no reason refuses the same file. */
        assert(profile_read_existing(&profile, "reasons") == -1);
    }
    /* A syntax error is libyaml's own text and the line it stopped at. */
    assert(fs_write_file_atomic(path, "cpus: \"4\n", 9, 0600) == 0);
    assert(profile_read_existing_reason(&profile, "reasons", reason) == -1);
    assert(strstr(reason, " at line ") && printable(reason));
    assert(unlink(path) == 0 && mkdir(path, 0700) == 0);
    assert(profile_read_existing_reason(&profile, "reasons", reason) == -1);
    assert(errno == EINVAL);
    assert(strcmp(reason, "config.yaml is not a regular file") == 0);
    assert(rmdir(path) == 0);
    static char oversized[64 * 1024 + 1];
    memset(oversized, '#', sizeof(oversized));
    assert(fs_write_file_atomic(path, oversized, sizeof(oversized), 0600) == 0);
    assert(profile_read_existing_reason(&profile, "reasons", reason) == -1);
    assert(strcmp(reason, "config.yaml is larger than 65536 bytes") == 0);
    assert(profile_read_existing_reason(&profile, "../escape", reason) == -1);
    assert(errno == EINVAL && strcmp(reason, "invalid profile name") == 0);
    /* An accepted configuration leaves no reason. */
    assert(fs_write_file_atomic(path, "cpus: 2\n", 8, 0600) == 0);
    memset(reason, 'x', sizeof(reason));
    assert(profile_read_existing_reason(&profile, "reasons", reason) == 0);
    assert(reason[0] == '\0' && profile.cpus == 2 && profile.mem_mib == 4096);
    assert(unlink(path) == 0 && rmdir(directory) == 0);
}

static struct profile with_mount(struct profile profile, size_t index,
                                 const char *location, const char *mount_point,
                                 int writable)
{
    struct profile_mount *mount = &profile.mounts[index];
    snprintf(mount->location, sizeof(mount->location), "%s", location);
    snprintf(mount->mount_point, sizeof(mount->mount_point), "%s", mount_point);
    mount->writable = writable;
    if (profile.mount_count <= index)
        profile.mount_count = index + 1;
    return profile;
}

static struct profile with_hook(struct profile profile, const char *stage,
                                const char *command, unsigned timeout_seconds,
                                int warn)
{
    struct profile_hook *hook = &profile.hooks[0];
    snprintf(hook->stage, sizeof(hook->stage), "%s", stage);
    snprintf(hook->command, sizeof(hook->command), "%s", command);
    hook->timeout_seconds = timeout_seconds;
    hook->warn = warn;
    profile.hook_count = 1;
    return profile;
}

/* profile_diff names exactly the settings that differ, in both directions. */
static void differing_settings_are_named(void)
{
    static const char *const keys[PROFILE_SETTING_COUNT] = {
        "cpus", "memoryMiB", "diskGiB", "mountHome", "homeReadOnly",
        "mountInotify", "docker.daemonJson", "rosetta",
        "nestedVirtualization", "sshAgent", "mounts", "provision",
    };
    for (int setting = 0; setting < PROFILE_SETTING_COUNT; setting++)
        assert(strcmp(profile_setting_key(setting), keys[setting]) == 0);
    assert(profile_setting_key(PROFILE_SETTING_COUNT) == NULL);
    assert(profile_setting_key((enum profile_setting)-1) == NULL);

    static struct profile base, changed[PROFILE_SETTING_COUNT];
    memset(&base, 0, sizeof(base));
    base.cpus = 4;
    base.mem_mib = 4096;
    base.disk_gib = 60;
    base.mount_home = 1;
    base = with_mount(base, 0, "/a", "/workspace/a", 0);
    base = with_hook(base, "ready", "echo ready", 60, 0);
    for (int setting = 0; setting < PROFILE_SETTING_COUNT; setting++)
        changed[setting] = base;
    changed[PROFILE_SETTING_CPUS].cpus = 5;
    changed[PROFILE_SETTING_MEMORY_MIB].mem_mib = 4097;
    changed[PROFILE_SETTING_DISK_GIB].disk_gib = 61;
    changed[PROFILE_SETTING_MOUNT_HOME].mount_home = 0;
    changed[PROFILE_SETTING_HOME_READ_ONLY].home_read_only = 1;
    changed[PROFILE_SETTING_MOUNT_INOTIFY].mount_inotify = 1;
    snprintf(changed[PROFILE_SETTING_DOCKER_DAEMON_JSON].docker_daemon_json,
             sizeof(base.docker_daemon_json), "{}");
    changed[PROFILE_SETTING_ROSETTA].rosetta = 1;
    changed[PROFILE_SETTING_NESTED_VIRTUALIZATION].nested_virtualization = 1;
    changed[PROFILE_SETTING_SSH_AGENT].ssh_agent = 1;
    changed[PROFILE_SETTING_MOUNTS].mounts[0].writable = 1;
    changed[PROFILE_SETTING_PROVISION].hooks[0].warn = 1;
    assert(profile_diff(&base, &base) == 0);
    for (int setting = 0; setting < PROFILE_SETTING_COUNT; setting++) {
        assert(profile_diff(&base, &changed[setting]) == 1u << setting);
        assert(profile_diff(&changed[setting], &base) == 1u << setting);
    }

    /* The name and directory are not settings; a boolean is true or false. */
    static struct profile other;
    other = base;
    snprintf(other.name, sizeof(other.name), "other");
    snprintf(other.dir, sizeof(other.dir), "/elsewhere");
    other.mount_home = 2;
    assert(profile_diff(&base, &other) == 0);

    /* Every field of a mount counts, and so do the number and the order. */
    const unsigned mounts = 1u << PROFILE_SETTING_MOUNTS;
    other = with_mount(base, 0, "/b", "/workspace/a", 0);
    assert(profile_diff(&base, &other) == mounts);
    other = with_mount(base, 0, "/a", "/workspace/b", 0);
    assert(profile_diff(&base, &other) == mounts);
    other = with_mount(base, 1, "/b", "/workspace/b", 0);
    assert(profile_diff(&base, &other) == mounts);
    static struct profile reordered;
    reordered = with_mount(base, 0, "/b", "/workspace/b", 0);
    reordered = with_mount(reordered, 1, "/a", "/workspace/a", 0);
    assert(profile_diff(&other, &reordered) == mounts);
    other = base;
    other.mount_count = 0;
    assert(profile_diff(&base, &other) == mounts);

    /* Every field of a hook counts. */
    const unsigned provision = 1u << PROFILE_SETTING_PROVISION;
    other = with_hook(base, "system", "echo ready", 60, 0);
    assert(profile_diff(&base, &other) == provision);
    other = with_hook(base, "ready", "echo other", 60, 0);
    assert(profile_diff(&base, &other) == provision);
    other = with_hook(base, "ready", "echo ready", 61, 0);
    assert(profile_diff(&base, &other) == provision);
    other = base;
    other.hook_count = 0;
    assert(profile_diff(&base, &other) == provision);

    /* Several differing settings are all named. */
    other = changed[PROFILE_SETTING_CPUS];
    other.rosetta = 1;
    other.mount_count = 0;
    assert(profile_diff(&base, &other) ==
           (1u << PROFILE_SETTING_CPUS | 1u << PROFILE_SETTING_ROSETTA | mounts));
}

int main(void)
{
    char temporary[] = "/tmp/hamn-profile-read-XXXXXX";
    assert(mkdtemp(temporary));
    assert(setenv("HOME", temporary, 1) == 0);
    struct profile profile;
    assert(profile_read_existing(&profile, "missing") == -1);
    assert(errno == ENOENT);
    char root[1024];
    assert(hamn_home(root, sizeof(root)));
    assert(access(root, F_OK) == -1);
    char *json = NULL;
    assert(hamn_control_query(NULL, &json) == 0);
    cJSON *items = cJSON_Parse(json);
    assert(cJSON_IsArray(items) && cJSON_GetArraySize(items) == 0);
    cJSON_Delete(items);
    hamn_control_free(json);
    assert(access(root, F_OK) == -1);
    assert(profile_read_existing(&profile, "../escape") == -1);
    assert(errno == EINVAL);
    assert(profile_load(&profile, "existing") == 0);
    assert(profile_save(&profile) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    refused_configurations_say_why(root);
    differing_settings_are_named();
    assert(hamn_control_start("../escape", 0, 0, 0) == 2);
    assert(hamn_control_configure("existing", 2, 2, 0, 0, -1) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    assert(profile.cpus == 2 && profile.mem_mib == 2048);
    assert(profile.disk_gib == 60);
    assert(hamn_control_configure("existing", 0, UINT_MAX, 0, 0, -1) == 2);
    assert(profile_read_existing(&profile, "existing") == 0);
    assert(profile.mem_mib == 2048);
    /* Rosetta: -1 keeps the setting, 0 and 1 set it, anything else is invalid. */
    assert(profile.rosetta == 0);
    assert(hamn_control_configure("existing", 0, 0, 0, 0, 1) == 0);
    assert(profile_read_existing(&profile, "existing") == 0 && profile.rosetta == 1);
    assert(hamn_control_configure("existing", 3, 0, 0, 0, -1) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    assert(profile.rosetta == 1 && profile.cpus == 3);
    assert(hamn_control_configure("existing", 0, 0, 0, 0, 2) == 2);
    assert(hamn_control_configure("existing", 0, 0, 0, 0, -2) == 2);
    assert(profile_read_existing(&profile, "existing") == 0 && profile.rosetta == 1);
    assert(hamn_control_configure("existing", 2, 0, 0, 0, 0) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    assert(profile.rosetta == 0 && profile.cpus == 2);
    /* The managed K3s `kubernetes` key was removed: a profile that still has
     * it is rejected rather than read with the key silently dropped. */
    char path[1024];
    assert(profile_path(&profile, "config.yaml", path, sizeof(path)));
    FILE *saved = fopen(path, "r");
    assert(saved);
    char original[8192];
    size_t length = fread(original, 1, sizeof(original), saved);
    assert(length > 0 && length < sizeof(original) && fclose(saved) == 0);
    FILE *legacy = fopen(path, "a");
    assert(legacy && fputs("kubernetes:\n  enabled: true\n", legacy) >= 0 && fclose(legacy) == 0);
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(fs_write_file_atomic(path, original, length, 0600) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    /* A config.yaml that is a directory, or is over the size limit, is
     * invalid. It is never reported as missing, whatever errno held before:
     * ENOENT means that the profile has no configuration. */
    assert(unlink(path) == 0 && mkdir(path, 0700) == 0);
    errno = ENOENT;
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(errno == EINVAL);
    assert(rmdir(path) == 0);
    static char oversized[64 * 1024 + 1];
    memset(oversized, '#', sizeof(oversized));
    assert(fs_write_file_atomic(path, oversized, sizeof(oversized), 0600) == 0);
    errno = ENOENT;
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(errno == EINVAL);
    /* At the limit the file is read: a comment alone is no configuration. */
    assert(fs_write_file_atomic(path, oversized, sizeof(oversized) - 1, 0600) == 0);
    errno = 0;
    assert(profile_read_existing(&profile, "existing") == -1 && errno == EINVAL);
    assert(fs_write_file_atomic(path, original, length, 0600) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    assert(hamn_control_query("existing", &json) == 0);
    items = cJSON_Parse(json);
    assert(cJSON_IsObject(items));
    assert(cJSON_IsNumber(cJSON_GetObjectItem(items, "cpus")));
    cJSON_Delete(items);
    hamn_control_free(json);
    char config[1024];
    assert(profile_path(&profile, "config.yaml", config, sizeof(config)));
    assert(unlink(config) == 0);
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(errno == ENOENT);
    assert(rmdir(profile.dir) == 0);
    snprintf(config, sizeof(config), "%s/.existing-mutation.lock", root);
    assert(unlink(config) == 0);
    snprintf(config, sizeof(config), "%s/.locks/existing.lock", root);
    assert(unlink(config) == 0);
    snprintf(config, sizeof(config), "%s/.locks", root);
    assert(rmdir(config) == 0);
    assert(rmdir(root) == 0);
    char foreign[1024];
    snprintf(foreign, sizeof(foreign), "%s/foreign", temporary);
    assert(mkdir(foreign, 0700) == 0);
    assert(symlink(foreign, root) == 0);
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(hamn_control_query(NULL, &json) == -1);
    assert(hamn_control_configure("escape", 2, 2, 0, 1, -1) != 0);
    assert(unlink(root) == 0);
    assert(mkdir(root, 0700) == 0);
    snprintf(config, sizeof(config), "%s/escape", root);
    assert(symlink(foreign, config) == 0);
    assert(profile_load(&profile, "escape") == -1);
    assert(unlink(config) == 0);
    snprintf(config, sizeof(config), "%s/.locks", root);
    assert(symlink(foreign, config) == 0);
    assert(hamn_control_configure("escape", 2, 2, 0, 1, -1) != 0);
    assert(unlink(config) == 0);
    assert(rmdir(root) == 0);
    assert(rmdir(foreign) == 0); /* No state or lock was created in the target. */
    assert(rmdir(temporary) == 0);
    puts("read-only profiles: passed");
    return 0;
}
