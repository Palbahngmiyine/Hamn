#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#include "core/profile.h"
#include "core/control.h"
#include "core/log.h"
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

static size_t read_bytes(const char *path, char *data, size_t capacity)
{
    FILE *file = fopen(path, "r");
    assert(file);
    size_t length = fread(data, 1, capacity, file);
    assert(length < capacity && fclose(file) == 0);
    return length;
}

/* config.yaml cannot carry every character: the YAML reader refuses some
 * and folds others into a line break. A profile with such a setting is not
 * saved, because the file would no longer read as that profile: every
 * operation on it would be refused until the file is corrected by hand. The
 * profile `existing` is left as it was. */
static void settings_that_would_not_read_back_are_not_saved(void)
{
    static const char *const refused[] = {
        "\x07",          /* BEL: a control character that is not written */
        "\x1b",          /* ESC, likewise */
        "\x7f",          /* DEL: refused by the reader */
        "\xc2\x80",      /* U+0080, a C1 control: refused by the reader */
        "\xef\xbf\xbe",  /* U+FFFE: refused by the reader */
        "\xc2\x85",      /* U+0085: read back as a space */
        "\xe2\x80\xa8 ", /* U+2028 and a space: read back without the space */
    };
    static const char *const saved[] = {
        "printf 'a\\tb\\n' \"$HOME\"",
        "tab\there, line\nbreak, carriage\rreturn",
        "\xea\xb2\xbd\xeb\xa1\x9c /Volumes/\xed\x95\x9c\xea\xb8\x80",
        "  spaces kept  ",
        "# no comment, - no list, {no: map}",
        /* U+2028 between two letters is read back as it is. */
        "line\xe2\x80\xa8separator",
    };
    static struct profile stored, changed, check;
    char path[1024], before[8192], after[8192];
    assert(profile_read_existing(&stored, "existing") == 0);
    assert(profile_path(&stored, "config.yaml", path, sizeof(path)));
    size_t length = read_bytes(path, before, sizeof(before));
    for (size_t index = 0; index < sizeof(refused) / sizeof(refused[0]); index++) {
        char value[64];
        snprintf(value, sizeof(value), "echo a%sb", refused[index]);
        changed = with_hook(stored, "ready", value, 60, 0);
        errno = 0;
        assert(profile_save(&changed) == -1);
        assert(errno == EILSEQ);
        snprintf(value, sizeof(value), "/data/a%sb", refused[index]);
        changed = with_mount(stored, 0, value, "/workspace", 0);
        errno = 0;
        assert(profile_save(&changed) == -1 && errno == EILSEQ);
        assert(read_bytes(path, after, sizeof(after)) == length &&
               memcmp(before, after, length) == 0);
    }
    for (size_t index = 0; index < sizeof(saved) / sizeof(saved[0]); index++) {
        changed = with_hook(stored, "ready", saved[index], 60, 0);
        assert(profile_save(&changed) == 0);
        assert(profile_read_existing(&check, "existing") == 0);
        assert(profile_diff(&changed, &check) == 0);
        assert(strcmp(check.hooks[0].command, saved[index]) == 0);
    }
    /* A YAML escape names such a character in a hand-written file, which
     * reads. `vm configure` saves what it read: it is refused, and the file
     * that still reads is kept. */
    const char *escaped = "provision:\n  - command: \"echo \\x7F\"\n";
    assert(fs_write_file_atomic(path, escaped, strlen(escaped), 0600) == 0);
    assert(profile_read_existing(&check, "existing") == 0);
    assert(strcmp(check.hooks[0].command, "echo \x7f") == 0);
    assert(hamn_control_configure("existing", 3, 0, 0, 0, -1) == 1);
    assert(read_bytes(path, after, sizeof(after)) == strlen(escaped) &&
           memcmp(after, escaped, strlen(escaped)) == 0);
    assert(profile_read_existing(&check, "existing") == 0 && check.cpus == 4);
    char *json = NULL;
    assert(hamn_control_query(NULL, &json) == 0);
    hamn_control_free(json);
    assert(profile_save(&stored) == 0);
    assert(read_bytes(path, after, sizeof(after)) == length &&
           memcmp(before, after, length) == 0);
}

/* The operation recorded exactly this sentence as its reason. */
static int said(const char *sentence)
{
    if (strcmp(log_last_error(), sentence) == 0)
        return 1;
    fprintf(stderr, "expected \"%s\", recorded \"%s\"\n", sentence,
            log_last_error());
    return 0;
}

static int start_with_a_smaller_disk(const char *profile)
{
    /* A start that got past the read would be refused here, before it saves
     * the profile or prepares an image: the disk of a profile cannot shrink
     * to 1 GiB. */
    return hamn_control_start(profile, 0, 0, 1);
}

/* Whether call(profile) returns 1 and records sentence. It runs in a child:
 * a call that ends the process instead of returning must fail this check
 * and not end the test, and its exit status 1 must not pass for the result
 * 1. The child gives up after 20 seconds. */
static int refuses_with(int (*call)(const char *), const char *profile,
                        const char *sentence)
{
    fflush(NULL);
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        alarm(20);
        log_clear_error();
        int rc = call(profile);
        _exit(rc == 1 && strcmp(log_last_error(), sentence) == 0 ? 0 : 3);
    }
    int status = 0;
    assert(waitpid(child, &status, 0) == child);
    if (WIFEXITED(status) && WEXITSTATUS(status) == 0)
        return 1;
    fprintf(stderr, "the call did not return 1 with \"%s\" (wait status %d)\n",
            sentence, status);
    return 0;
}

/* Each operation that needs the stored configuration says why it cannot
 * read it: the rule the file breaks, or that there is no such profile. The
 * configuration of `existing` is unreadable during the call, with the removed
 * kubernetes mapping, and holds length bytes that no operation changes. */
static void operations_say_why_a_profile_cannot_be_read(const char *root,
                                                        const char *path)
{
    static const char unreadable[] = "cannot read the configuration of "
        "profile existing: unknown configuration key: kubernetes";
    static const char absent[] = "profile absent does not exist";
    char before[8192], after[8192], archive[1100], other[1100];
    size_t length = read_bytes(path, before, sizeof(before));
    snprintf(archive, sizeof(archive), "%s/diagnostics.tar", root);
    char *json = (char *)"";

    /* A status query records the sentence; the caller reports it. */
    log_clear_error();
    assert(hamn_control_query("existing", &json) == -1 && json == NULL);
    assert(said(unreadable));
    log_clear_error();
    assert(hamn_control_configure("existing", 3, 0, 0, 0, -1) == 1);
    assert(said(unreadable));
    log_clear_error();
    assert(hamn_control_stop("existing") == 1);
    assert(said(unreadable));
    log_clear_error();
    json = (char *)"";
    assert(hamn_control_diagnostics("existing", archive, &json) == 1);
    assert(json == NULL && said(unreadable));
    assert(access(archive, F_OK) == -1 && errno == ENOENT);
    /* A start and a delete answer too, and leave the profile as it is: no
     * operation record, and no marker of a deleted profile. */
    assert(refuses_with(start_with_a_smaller_disk, "existing", unreadable));
    assert(refuses_with(hamn_control_delete, "existing", unreadable));
    snprintf(other, sizeof(other), "%s/existing/operation.json", root);
    assert(access(other, F_OK) == -1 && errno == ENOENT);
    snprintf(other, sizeof(other), "%s/existing/deleted", root);
    assert(access(other, F_OK) == -1 && errno == ENOENT);
    assert(read_bytes(path, after, sizeof(after)) == length &&
           memcmp(before, after, length) == 0);

    /* A name that no profile has is not a configuration that cannot be
     * read, and asking about it creates no profile. */
    log_clear_error();
    assert(hamn_control_query("absent", &json) == -1 && said(absent));
    log_clear_error();
    assert(hamn_control_configure("absent", 3, 0, 0, 0, -1) == 1);
    assert(said(absent));
    log_clear_error();
    assert(hamn_control_stop("absent") == 1 && said(absent));
    log_clear_error();
    assert(hamn_control_diagnostics("absent", archive, &json) == 1);
    assert(said(absent) && access(archive, F_OK) == -1);
    snprintf(other, sizeof(other), "%s/absent", root);
    assert(access(other, F_OK) == -1 && errno == ENOENT);
    snprintf(other, sizeof(other), "%s/.locks/absent.lock", root);
    assert(unlink(other) == 0);

    /* A profile that cannot be created had no configuration to read: the
     * reason is the directory. Root would create it all the same. */
    if (geteuid() != 0) {
        assert(chmod(root, 0500) == 0);
        log_clear_error();
        assert(hamn_control_configure("fresh", 2, 2, 0, 1, -1) == 1);
        assert(said("cannot create profile fresh: Permission denied"));
        assert(chmod(root, 0700) == 0);
        snprintf(other, sizeof(other), "%s/fresh", root);
        assert(access(other, F_OK) == -1 && errno == ENOENT);
        snprintf(other, sizeof(other), "%s/.locks/fresh.lock", root);
        assert(unlink(other) == 0);
    }
}

/* A config.yaml whose path does not fit is an error with an errno of its
 * own. The reader used to leave errno as an earlier call had set it, and a
 * leftover ENOENT reported the profile as one that does not exist. HOME is
 * as long as it can be with the profile directory still fitting: 1004 bytes,
 * for the profile `p` and paths of PROFILE_PATH_CAP bytes. */
static void a_configuration_path_that_does_not_fit_says_so(const char *base)
{
    enum { HOME_LENGTH = PROFILE_PATH_CAP - (sizeof("/.hamn/p/config.yaml") - 1) };
    static char home[PROFILE_PATH_CAP], saved[PROFILE_PATH_CAP];
    const char *previous = getenv("HOME");
    assert(previous && strlen(previous) < sizeof(saved));
    strcpy(saved, previous);
    /* Nested directories of at most 200 bytes each, the last one shorter. */
    size_t length = (size_t)snprintf(home, sizeof(home), "%s/long", base);
    assert(mkdir(home, 0700) == 0);
    while (length < HOME_LENGTH) {
        size_t part = HOME_LENGTH - length - 1;
        if (part > 200)
            part = 200;
        if (part == 0)
            break;
        home[length++] = '/';
        memset(home + length, 'd', part);
        length += part;
        home[length] = '\0';
        assert(mkdir(home, 0700) == 0);
    }
    assert(strlen(home) == HOME_LENGTH);
    assert(setenv("HOME", home, 1) == 0);

    struct profile located, profile;
    char path[PROFILE_PATH_CAP], reason[PROFILE_REASON_CAP];
    assert(hamn_home(path, sizeof(path)) && mkdir(path, 0700) == 0);
    memset(&located, 0, sizeof(located));
    snprintf(located.name, sizeof(located.name), "p");
    assert(profile_locate(&located) == 0 && mkdir(located.dir, 0700) == 0);
    /* The directory is found and is private; only the file's path is one
     * byte too long. */
    assert(profile_directory_private(&located) == 1);
    assert(!profile_path(&located, "config.yaml", path, sizeof(path)));
    errno = ENOENT;
    assert(profile_read_existing_reason(&profile, "p", reason) == -1);
    assert(errno == ENAMETOOLONG);
    assert(strcmp(reason, strerror(ENAMETOOLONG)) == 0);

    assert(rmdir(located.dir) == 0);
    assert(hamn_home(path, sizeof(path)) && rmdir(path) == 0);
    assert(setenv("HOME", saved, 1) == 0);
    /* Innermost first, down to the directory this case made in base. */
    size_t stop = strlen(base) + sizeof("/long") - 1;
    for (;;) {
        assert(rmdir(home) == 0);
        if (strlen(home) <= stop)
            break;
        *strrchr(home, '/') = '\0';
    }
}

/* The list in a child that has one descriptor left: enough to read the
 * directory of profiles, not to open a config.yaml. Exits 0 when the list
 * fails as a whole and says that a configuration could not be read for want
 * of descriptors, and 3 when it reports anything else, such as every
 * profile as unreadable. */
static int the_list_fails_without_descriptors(void)
{
    fflush(NULL);
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        alarm(20);
        /* The lowest free descriptor: the limit below leaves it alone. */
        int next = open("/dev/null", O_RDONLY);
        struct rlimit limit;
        if (next < 0 || close(next) != 0 ||
            getrlimit(RLIMIT_NOFILE, &limit) != 0)
            _exit(4);
        limit.rlim_cur = (rlim_t)next + 1;
        if (setrlimit(RLIMIT_NOFILE, &limit) != 0)
            _exit(4);
        char *json = NULL;
        int rc = hamn_control_query(NULL, &json);
        _exit(rc == -1 && !json &&
              strstr(log_last_error(),
                     "cannot read the configuration of profile ") ==
                  log_last_error() &&
              strstr(log_last_error(), ": Too many open files") ? 0 : 3);
    }
    int status = 0;
    assert(waitpid(child, &status, 0) == child);
    if (WIFEXITED(status) && WEXITSTATUS(status) == 0)
        return 1;
    fprintf(stderr, "the list without descriptors: wait status %d\n", status);
    return 0;
}

static const char *text_of(const cJSON *object, const char *key)
{
    const cJSON *item = cJSON_GetObjectItemCaseSensitive(object, key);
    return cJSON_IsString(item) ? item->valuestring : "";
}

/* One profile whose configuration cannot be read does not hide the others.
 * The list names it with the state of its VM, its directory and the reason,
 * and with nothing that comes from the file it could not read; the profile
 * beside it is listed in full. `existing` is unreadable during the call,
 * with the removed kubernetes mapping; `other` is created and removed. */
static void an_unreadable_profile_is_listed_beside_the_others(const char *root)
{
    struct profile other;
    char directory[1100], path[1200];
    assert(profile_load(&other, "other") == 0 && profile_save(&other) == 0);
    snprintf(directory, sizeof(directory), "%s/existing", root);

    char *json = NULL;
    log_set_error("the reason of an earlier call");
    assert(hamn_control_query(NULL, &json) == 0);
    /* The list forgets the reason of an earlier call. */
    assert(log_last_error()[0] == '\0');
    cJSON *rows = cJSON_Parse(json);
    assert(cJSON_IsArray(rows) && cJSON_GetArraySize(rows) == 2);
    int seen = 0;
    const cJSON *row;
    cJSON_ArrayForEach(row, rows) {
        if (strcmp(text_of(row, "name"), "existing") == 0) {
            assert(cJSON_GetArraySize(row) == 4);
            assert(strcmp(text_of(row, "state"), "stopped") == 0);
            assert(strcmp(text_of(row, "directory"), directory) == 0);
            assert(strcmp(text_of(row, "configurationError"),
                          "unknown configuration key: kubernetes") == 0);
            seen |= 1;
        } else {
            assert(strcmp(text_of(row, "name"), "other") == 0);
            assert(cJSON_IsNumber(cJSON_GetObjectItem(row, "cpus")));
            assert(cJSON_IsString(cJSON_GetObjectItem(row, "dockerSocket")));
            assert(!cJSON_GetObjectItem(row, "configurationError"));
            seen |= 2;
        }
    }
    assert(seen == 3);
    cJSON_Delete(rows);
    hamn_control_free(json);

    /* The list still fails as a whole, with the profile named, in the cases
     * that are not about what a file says. A profile directory that others
     * can write is not looked into: */
    assert(chmod(directory, 0770) == 0);
    assert(hamn_control_query(NULL, &json) == -1 && json == NULL);
    assert(said("cannot read the configuration of profile existing: the "
                "profile directory is not a directory that only this user "
                "can write"));
    assert(chmod(directory, 0700) == 0);
    /* a read that ran out of descriptors learned nothing about the file: */
    assert(the_list_fails_without_descriptors());
    /* and a state file that is not one belongs to a profile that reads. */
    assert(profile_path(&other, "state.json", path, sizeof(path)));
    assert(fs_write_file_atomic(path, "{", 1, 0600) == 0);
    assert(hamn_control_query(NULL, &json) == -1 && json == NULL);
    assert(strstr(log_last_error(), "is not valid JSON"));
    assert(hamn_control_query("other", &json) == -1 && json == NULL);
    assert(unlink(path) == 0);

    assert(profile_path(&other, "config.yaml", path, sizeof(path)));
    assert(unlink(path) == 0 && rmdir(other.dir) == 0);
}

/* A query reports its own failure only. It forgets the reason an earlier
 * call recorded, so that a failure it has no sentence for is not reported
 * with the sentence of another operation. `existing` reads. */
static void a_query_forgets_the_reason_of_an_earlier_call(void)
{
    char *json = NULL;
    assert(hamn_control_configure("existing", 0, 0, 1, 0, -1) == 1);
    assert(strstr(log_last_error(), "disk size cannot shrink"));
    assert(hamn_control_query("existing", &json) == 0);
    hamn_control_free(json);
    assert(log_last_error()[0] == '\0');
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
    a_configuration_path_that_does_not_fit_says_so(temporary);
    differing_settings_are_named();
    settings_that_would_not_read_back_are_not_saved();
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
    operations_say_why_a_profile_cannot_be_read(root, path);
    an_unreadable_profile_is_listed_beside_the_others(root);
    assert(fs_write_file_atomic(path, original, length, 0600) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    a_query_forgets_the_reason_of_an_earlier_call();
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
    /* At the limit the file is read: its settings, then comment. */
    memcpy(oversized, "cpus: 7\n", 8);
    assert(fs_write_file_atomic(path, oversized, sizeof(oversized) - 1, 0600) == 0);
    assert(profile_read_existing(&profile, "existing") == 0 && profile.cpus == 7);
    /* A FIFO is no configuration either, and opening it does not wait for
     * a writer: without one, a blocking open would never return. */
    assert(unlink(path) == 0 && mkfifo(path, 0600) == 0);
    alarm(10);
    errno = ENOENT;
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(errno == EINVAL);
    alarm(0);
    assert(unlink(path) == 0);
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
    /* A list that fails for a reason without a sentence reports none. */
    log_set_error("the reason of an earlier call");
    assert(hamn_control_query(NULL, &json) == -1);
    assert(log_last_error()[0] == '\0');
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
