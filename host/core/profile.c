#include "core/profile.h"

#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include <yaml.h>

#include "cjson/cJSON.h"
#include "util/fs.h"

#define PROFILE_CONFIG_FILE "config.yaml"
#define PROFILE_YAML_CAP (64 * 1024)
#define PROFILE_SEEN_KEY_CAP 16

struct yaml_parse {
    yaml_parser_t parser;
    char error[PROFILE_REASON_CAP];
};

struct yaml_text {
    char data[PROFILE_YAML_CAP];
    size_t length;
    int unwritable; /* a string held a character that text_quote refuses */
};

static void profile_defaults(struct profile *profile)
{
    memset(profile, 0, sizeof(*profile));
    profile->cpus = 4;
    profile->mem_mib = 4096;
    profile->disk_gib = 60;
    profile->mount_home = 1;
}

int profile_name_valid(const char *name)
{
    if (!name || !name[0] || strlen(name) >= PROFILE_NAME_CAP)
        return 0;
    for (const unsigned char *cursor = (const unsigned char *)name; *cursor;
         cursor++) {
        if (!(isalnum(*cursor) || *cursor == '-' || *cursor == '_'))
            return 0;
    }
    return strcmp(name, ".") != 0 && strcmp(name, "..") != 0 &&
           strcmp(name, "cache") != 0;
}

int profile_parse_positive(const char *text, unsigned *out)
{
    if (!text || !*text || text[0] == '-')
        return -1;
    char *end = NULL;
    errno = 0;
    unsigned long value = strtoul(text, &end, 10);
    if (errno || !end || *end || value == 0 || value > UINT_MAX)
        return -1;
    *out = (unsigned)value;
    return 0;
}

static int json_keys_unique(const cJSON *value)
{
    if (cJSON_IsObject(value)) {
        for (const cJSON *child = value->child; child; child = child->next) {
            if (!child->string || !json_keys_unique(child))
                return 0;
            for (const cJSON *other = child->next; other; other = other->next) {
                if (other->string && strcmp(child->string, other->string) == 0)
                    return 0;
            }
        }
    } else if (cJSON_IsArray(value)) {
        for (const cJSON *child = value->child; child; child = child->next) {
            if (!json_keys_unique(child))
                return 0;
        }
    }
    return 1;
}

int profile_docker_daemon_json_valid(const char *text)
{
    if (!text || !text[0])
        return 1;
    cJSON *json = cJSON_ParseWithOpts(text, NULL, 1);
    if (!cJSON_IsObject(json) || !json_keys_unique(json)) {
        cJSON_Delete(json);
        return 0;
    }
    static const char *const reserved[] = {
        "containerd", "host-gateway-ip", "hosts", "data-root", "exec-root",
        NULL,
    };
    for (size_t index = 0; reserved[index]; index++) {
        if (cJSON_GetObjectItemCaseSensitive(json, reserved[index])) {
            cJSON_Delete(json);
            return 0;
        }
    }
    cJSON *features = cJSON_GetObjectItemCaseSensitive(json, "features");
    int valid = !features ||
        (cJSON_IsObject(features) &&
         (!cJSON_GetObjectItemCaseSensitive(features, "buildkit") ||
          cJSON_IsTrue(cJSON_GetObjectItemCaseSensitive(features, "buildkit"))));
    cJSON_Delete(json);
    return valid;
}

const char *hamn_home(char *buf, size_t cap)
{
    const char *home = getenv("HOME");
    if (!home || !home[0] || snprintf(buf, cap, "%s/.hamn", home) >=
        (int)cap)
        return NULL;
    struct stat st;
    if (lstat(buf, &st) == 0) {
        if (!S_ISDIR(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 0022)) {
            errno = EPERM;
            return NULL;
        }
    } else if (errno != ENOENT) {
        return NULL;
    }
    return buf;
}

const char *profile_path(const struct profile *profile, const char *file,
                         char *buf, size_t cap)
{
    if (!profile || !file || !buf ||
        snprintf(buf, cap, "%s/%s", profile->dir, file) >= (int)cap)
        return NULL;
    return buf;
}

static void yaml_fail(struct yaml_parse *parse, const char *format, ...)
{
    if (parse->error[0])
        return;
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(parse->error, sizeof(parse->error), format, arguments);
    va_end(arguments);
    /* A reason can quote a key of the file and is shown on a terminal. */
    for (unsigned char *cursor = (unsigned char *)parse->error; *cursor;
         cursor++) {
        if (*cursor < 0x20 || *cursor > 0x7e)
            *cursor = '?';
    }
}

static void reason_set(char *reason, const char *text)
{
    if (reason)
        snprintf(reason, PROFILE_REASON_CAP, "%s", text);
}

static int yaml_event_forbidden(struct yaml_parse *parse,
                                const yaml_event_t *event)
{
    switch (event->type) {
    case YAML_ALIAS_EVENT:
        yaml_fail(parse, "YAML aliases are not supported");
        return 1;
    case YAML_SCALAR_EVENT:
        if (event->data.scalar.anchor || event->data.scalar.tag) {
            yaml_fail(parse, "YAML anchors and tags are not supported");
            return 1;
        }
        return 0;
    case YAML_SEQUENCE_START_EVENT:
        if (event->data.sequence_start.anchor || event->data.sequence_start.tag) {
            yaml_fail(parse, "YAML anchors and tags are not supported");
            return 1;
        }
        return 0;
    case YAML_MAPPING_START_EVENT:
        if (event->data.mapping_start.anchor || event->data.mapping_start.tag) {
            yaml_fail(parse, "YAML anchors and tags are not supported");
            return 1;
        }
        return 0;
    default:
        return 0;
    }
}

static int yaml_next(struct yaml_parse *parse, yaml_event_t *event)
{
    memset(event, 0, sizeof(*event));
    if (!yaml_parser_parse(&parse->parser, event)) {
        const char *problem = parse->parser.problem ?
            parse->parser.problem : "invalid YAML";
        yaml_fail(parse, "%s at line %zu", problem,
                  parse->parser.problem_mark.line + 1);
        return -1;
    }
    if (yaml_event_forbidden(parse, event)) {
        yaml_event_delete(event);
        return -1;
    }
    return 0;
}

/* The next event, which must be of `type`. An event of another type is
 * released here, so that a failed call leaves the caller nothing to free. */
static int yaml_expect(struct yaml_parse *parse, yaml_event_t *event,
                       yaml_event_type_t type)
{
    if (yaml_next(parse, event) != 0)
        return -1;
    if (event->type != type) {
        yaml_event_delete(event);
        return -1;
    }
    return 0;
}

static int yaml_scalar(struct yaml_parse *parse, yaml_event_t *event,
                       char *out, size_t cap, int plain_only)
{
    if (event->type != YAML_SCALAR_EVENT) {
        yaml_fail(parse, "expected a scalar value");
        yaml_event_delete(event);
        return -1;
    }
    if ((plain_only && event->data.scalar.style != YAML_PLAIN_SCALAR_STYLE) ||
        event->data.scalar.length >= cap ||
        memchr(event->data.scalar.value, '\0', event->data.scalar.length)) {
        yaml_fail(parse, "invalid scalar value");
        yaml_event_delete(event);
        return -1;
    }
    memcpy(out, event->data.scalar.value, event->data.scalar.length);
    out[event->data.scalar.length] = '\0';
    yaml_event_delete(event);
    return 0;
}

static int yaml_string(struct yaml_parse *parse, yaml_event_t *event,
                       char *out, size_t cap)
{
    return yaml_scalar(parse, event, out, cap, 0);
}

static int yaml_bool(struct yaml_parse *parse, yaml_event_t *event, int *out)
{
    char text[16];
    if (yaml_scalar(parse, event, text, sizeof(text), 1) != 0)
        return -1;
    if (strcmp(text, "true") == 0) {
        *out = 1;
        return 0;
    }
    if (strcmp(text, "false") == 0) {
        *out = 0;
        return 0;
    }
    yaml_fail(parse, "expected boolean true or false");
    return -1;
}

static int yaml_positive(struct yaml_parse *parse, yaml_event_t *event,
                         unsigned *out)
{
    char text[32];
    if (yaml_scalar(parse, event, text, sizeof(text), 1) != 0)
        return -1;
    if (profile_parse_positive(text, out) != 0) {
        yaml_fail(parse, "expected a positive integer");
        return -1;
    }
    return 0;
}

static int yaml_mapping_start(struct yaml_parse *parse, yaml_event_t *event)
{
    if (event->type != YAML_MAPPING_START_EVENT) {
        yaml_fail(parse, "expected a mapping");
        yaml_event_delete(event);
        return -1;
    }
    yaml_event_delete(event);
    return 0;
}

static int yaml_sequence_start(struct yaml_parse *parse, yaml_event_t *event)
{
    if (event->type != YAML_SEQUENCE_START_EVENT) {
        yaml_fail(parse, "expected a sequence");
        yaml_event_delete(event);
        return -1;
    }
    yaml_event_delete(event);
    return 0;
}

static int seen_key(char keys[PROFILE_SEEN_KEY_CAP][64], size_t *count,
                    const char *key)
{
    for (size_t index = 0; index < *count; index++) {
        if (strcmp(keys[index], key) == 0)
            return -1;
    }
    if (*count == PROFILE_SEEN_KEY_CAP || strlen(key) >= sizeof(keys[0]))
        return -1;
    snprintf(keys[(*count)++], sizeof(keys[0]), "%s", key);
    return 0;
}

static int clean_absolute_path(const char *path, int allow_root)
{
    if (!path || path[0] != '/' || (!allow_root && strcmp(path, "/") == 0))
        return 0;
    const char *part = path + 1;
    while (*part) {
        const char *end = strchr(part, '/');
        size_t length = end ? (size_t)(end - part) : strlen(part);
        if (length == 0 || (length == 1 && part[0] == '.') ||
            (length == 2 && part[0] == '.' && part[1] == '.') ||
            memchr(part, '\n', length) || memchr(part, '\r', length))
            return 0;
        if (!end)
            break;
        part = end + 1;
    }
    return 1;
}

static int hook_stage_valid(const char *stage)
{
    return strcmp(stage, "system") == 0 || strcmp(stage, "user") == 0 ||
           strcmp(stage, "after-boot") == 0 || strcmp(stage, "ready") == 0;
}

/* The rule of the profile schema that the settings break, or NULL. */
static const char *profile_violation(const struct profile *profile)
{
    if (!profile)
        return "no profile";
    if (!profile->cpus || !profile->mem_mib || !profile->disk_gib)
        return "cpus, memoryMiB and diskGiB must be positive";
    if (profile->home_read_only && !profile->mount_home)
        return "homeReadOnly requires mountHome";
    if (!profile_docker_daemon_json_valid(profile->docker_daemon_json))
        return "docker.daemonJson must be one JSON object that leaves the "
               "settings Hamn manages alone";
    if (profile->mount_count > PROFILE_MAX_MOUNTS)
        return "too many mounts";
    if (profile->hook_count > PROFILE_MAX_HOOKS)
        return "too many provision hooks";
    int has_writable_share = profile->mount_home && !profile->home_read_only;
    for (size_t index = 0; index < profile->mount_count; index++) {
        const struct profile_mount *mount = &profile->mounts[index];
        if (!clean_absolute_path(mount->location, 1))
            return "a mount location must be a normalized absolute path";
        if (!clean_absolute_path(mount->mount_point, 0))
            return "a mountPoint must be a normalized absolute path other "
                   "than /";
        if (mount->writable)
            has_writable_share = 1;
        for (size_t other = 0; other < index; other++) {
            if (strcmp(mount->mount_point,
                       profile->mounts[other].mount_point) == 0)
                return "each mountPoint must be unique";
        }
    }
    if (profile->mount_inotify && !has_writable_share)
        return "mountInotify requires a writable share";
    for (size_t index = 0; index < profile->hook_count; index++) {
        const struct profile_hook *hook = &profile->hooks[index];
        if (!hook_stage_valid(hook->stage))
            return "a provision stage must be system, user, after-boot or "
                   "ready";
        if (!hook->command[0])
            return "a provision command must not be empty";
        if (hook->timeout_seconds == 0 || hook->timeout_seconds > 3600)
            return "a provision timeoutSeconds must be 1 to 3600";
    }
    return NULL;
}

static int profile_validate(const struct profile *profile)
{
    return profile_violation(profile) ? -1 : 0;
}

static int parse_docker(struct yaml_parse *parse, yaml_event_t *event,
                        struct profile *profile)
{
    if (yaml_mapping_start(parse, event) != 0)
        return -1;
    char seen[PROFILE_SEEN_KEY_CAP][64] = {{0}};
    size_t count = 0;
    for (;;) {
        yaml_event_t key_event;
        if (yaml_next(parse, &key_event) != 0)
            return -1;
        if (key_event.type == YAML_MAPPING_END_EVENT) {
            yaml_event_delete(&key_event);
            return 0;
        }
        char key[64];
        if (yaml_string(parse, &key_event, key, sizeof(key)) != 0 ||
            seen_key(seen, &count, key) != 0) {
            yaml_fail(parse, "duplicate or invalid docker key");
            return -1;
        }
        yaml_event_t value;
        if (yaml_next(parse, &value) != 0)
            return -1;
        if (strcmp(key, "daemonJson") != 0) {
            yaml_event_delete(&value);
            yaml_fail(parse, "unknown docker key: %s", key);
            return -1;
        }
        if (yaml_string(parse, &value, profile->docker_daemon_json,
                        sizeof(profile->docker_daemon_json)) != 0)
            return -1;
    }
}

static int parse_mount(struct yaml_parse *parse, yaml_event_t *event,
                       struct profile_mount *mount)
{
    if (yaml_mapping_start(parse, event) != 0)
        return -1;
    char seen[PROFILE_SEEN_KEY_CAP][64] = {{0}};
    size_t count = 0;
    int have_location = 0, have_mount_point = 0;
    for (;;) {
        yaml_event_t key_event;
        if (yaml_next(parse, &key_event) != 0)
            return -1;
        if (key_event.type == YAML_MAPPING_END_EVENT) {
            yaml_event_delete(&key_event);
            break;
        }
        char key[64];
        if (yaml_string(parse, &key_event, key, sizeof(key)) != 0 ||
            seen_key(seen, &count, key) != 0) {
            yaml_fail(parse, "duplicate or invalid mount key");
            return -1;
        }
        yaml_event_t value;
        if (yaml_next(parse, &value) != 0)
            return -1;
        int rc;
        if (strcmp(key, "location") == 0) {
            have_location = 1;
            rc = yaml_string(parse, &value, mount->location,
                             sizeof(mount->location));
        } else if (strcmp(key, "mountPoint") == 0) {
            have_mount_point = 1;
            rc = yaml_string(parse, &value, mount->mount_point,
                             sizeof(mount->mount_point));
        } else if (strcmp(key, "writable") == 0) {
            rc = yaml_bool(parse, &value, &mount->writable);
        } else {
            yaml_event_delete(&value);
            yaml_fail(parse, "unknown mount key: %s", key);
            return -1;
        }
        if (rc != 0)
            return -1;
    }
    if (!have_location || !have_mount_point) {
        yaml_fail(parse, "each mount requires location and mountPoint");
        return -1;
    }
    return 0;
}

static int parse_mounts(struct yaml_parse *parse, yaml_event_t *event,
                        struct profile *profile)
{
    if (yaml_sequence_start(parse, event) != 0)
        return -1;
    profile->mount_count = 0;
    for (;;) {
        yaml_event_t item;
        if (yaml_next(parse, &item) != 0)
            return -1;
        if (item.type == YAML_SEQUENCE_END_EVENT) {
            yaml_event_delete(&item);
            return 0;
        }
        if (profile->mount_count == PROFILE_MAX_MOUNTS) {
            yaml_event_delete(&item);
            yaml_fail(parse, "too many mounts");
            return -1;
        }
        if (parse_mount(parse, &item,
                        &profile->mounts[profile->mount_count]) != 0)
            return -1;
        profile->mount_count++;
    }
}

static int parse_hook(struct yaml_parse *parse, yaml_event_t *event,
                      struct profile_hook *hook)
{
    if (yaml_mapping_start(parse, event) != 0)
        return -1;
    snprintf(hook->stage, sizeof(hook->stage), "ready");
    hook->timeout_seconds = 60;
    char seen[PROFILE_SEEN_KEY_CAP][64] = {{0}};
    size_t count = 0;
    int have_command = 0;
    for (;;) {
        yaml_event_t key_event;
        if (yaml_next(parse, &key_event) != 0)
            return -1;
        if (key_event.type == YAML_MAPPING_END_EVENT) {
            yaml_event_delete(&key_event);
            break;
        }
        char key[64];
        if (yaml_string(parse, &key_event, key, sizeof(key)) != 0 ||
            seen_key(seen, &count, key) != 0) {
            yaml_fail(parse, "duplicate or invalid provision key");
            return -1;
        }
        yaml_event_t value;
        if (yaml_next(parse, &value) != 0)
            return -1;
        int rc;
        if (strcmp(key, "stage") == 0)
            rc = yaml_string(parse, &value, hook->stage, sizeof(hook->stage));
        else if (strcmp(key, "command") == 0) {
            have_command = 1;
            rc = yaml_string(parse, &value, hook->command,
                             sizeof(hook->command));
        } else if (strcmp(key, "timeoutSeconds") == 0)
            rc = yaml_positive(parse, &value, &hook->timeout_seconds);
        else if (strcmp(key, "mode") == 0) {
            char mode[16];
            rc = yaml_string(parse, &value, mode, sizeof(mode));
            if (rc == 0) {
                if (strcmp(mode, "fail") != 0 && strcmp(mode, "warn") != 0) {
                    yaml_fail(parse, "provision mode must be fail or warn");
                    return -1;
                }
                hook->warn = strcmp(mode, "warn") == 0;
            }
        } else {
            yaml_event_delete(&value);
            yaml_fail(parse, "unknown provision key: %s", key);
            return -1;
        }
        if (rc != 0)
            return -1;
    }
    if (!have_command) {
        yaml_fail(parse, "each provision hook requires command");
        return -1;
    }
    return 0;
}

static int parse_provision(struct yaml_parse *parse, yaml_event_t *event,
                           struct profile *profile)
{
    if (yaml_sequence_start(parse, event) != 0)
        return -1;
    profile->hook_count = 0;
    for (;;) {
        yaml_event_t item;
        if (yaml_next(parse, &item) != 0)
            return -1;
        if (item.type == YAML_SEQUENCE_END_EVENT) {
            yaml_event_delete(&item);
            return 0;
        }
        if (profile->hook_count == PROFILE_MAX_HOOKS) {
            yaml_event_delete(&item);
            yaml_fail(parse, "too many provision hooks");
            return -1;
        }
        if (parse_hook(parse, &item,
                       &profile->hooks[profile->hook_count]) != 0)
            return -1;
        profile->hook_count++;
    }
}

static int parse_root(struct yaml_parse *parse, yaml_event_t *event,
                      struct profile *profile)
{
    if (yaml_mapping_start(parse, event) != 0)
        return -1;
    char seen[PROFILE_SEEN_KEY_CAP][64] = {{0}};
    size_t count = 0;
    for (;;) {
        yaml_event_t key_event;
        if (yaml_next(parse, &key_event) != 0)
            return -1;
        if (key_event.type == YAML_MAPPING_END_EVENT) {
            yaml_event_delete(&key_event);
            break;
        }
        char key[64];
        if (yaml_string(parse, &key_event, key, sizeof(key)) != 0 ||
            seen_key(seen, &count, key) != 0) {
            yaml_fail(parse, "duplicate or invalid configuration key");
            return -1;
        }
        yaml_event_t value;
        if (yaml_next(parse, &value) != 0)
            return -1;
        int rc;
        if (strcmp(key, "cpus") == 0)
            rc = yaml_positive(parse, &value, &profile->cpus);
        else if (strcmp(key, "memoryMiB") == 0)
            rc = yaml_positive(parse, &value, &profile->mem_mib);
        else if (strcmp(key, "diskGiB") == 0)
            rc = yaml_positive(parse, &value, &profile->disk_gib);
        else if (strcmp(key, "mountHome") == 0)
            rc = yaml_bool(parse, &value, &profile->mount_home);
        else if (strcmp(key, "homeReadOnly") == 0)
            rc = yaml_bool(parse, &value, &profile->home_read_only);
        else if (strcmp(key, "mountInotify") == 0)
            rc = yaml_bool(parse, &value, &profile->mount_inotify);
        else if (strcmp(key, "docker") == 0)
            rc = parse_docker(parse, &value, profile);
        else if (strcmp(key, "rosetta") == 0)
            rc = yaml_bool(parse, &value, &profile->rosetta);
        else if (strcmp(key, "nestedVirtualization") == 0)
            rc = yaml_bool(parse, &value, &profile->nested_virtualization);
        else if (strcmp(key, "sshAgent") == 0)
            rc = yaml_bool(parse, &value, &profile->ssh_agent);
        else if (strcmp(key, "mounts") == 0)
            rc = parse_mounts(parse, &value, profile);
        else if (strcmp(key, "provision") == 0)
            rc = parse_provision(parse, &value, profile);
        else {
            yaml_event_delete(&value);
            yaml_fail(parse, "unknown configuration key: %s", key);
            return -1;
        }
        if (rc != 0)
            return -1;
    }
    const char *violation = profile_violation(profile);
    if (violation) {
        yaml_fail(parse, "%s", violation);
        return -1;
    }
    return 0;
}

#define PROFILE_DEFINITION_API_VERSION "hamn/v1"
#define PROFILE_DEFINITION_KIND "Profile"

/* A scalar of a profile definition that must be exactly `expected`. */
static int parse_constant(struct yaml_parse *parse, yaml_event_t *event,
                          const char *key, const char *expected)
{
    char value[64];
    if (yaml_string(parse, event, value, sizeof(value)) != 0 ||
        strcmp(value, expected) != 0) {
        /* Not the value's own fault text: say what the key must be. */
        parse->error[0] = '\0';
        yaml_fail(parse, "%s must be %s", key, expected);
        return -1;
    }
    return 0;
}

static int parse_metadata(struct yaml_parse *parse, yaml_event_t *event,
                          struct profile *profile)
{
    if (yaml_mapping_start(parse, event) != 0)
        return -1;
    char seen[PROFILE_SEEN_KEY_CAP][64] = {{0}};
    size_t count = 0;
    for (;;) {
        yaml_event_t key_event;
        if (yaml_next(parse, &key_event) != 0)
            return -1;
        if (key_event.type == YAML_MAPPING_END_EVENT) {
            yaml_event_delete(&key_event);
            return 0;
        }
        char key[64];
        if (yaml_string(parse, &key_event, key, sizeof(key)) != 0 ||
            seen_key(seen, &count, key) != 0) {
            yaml_fail(parse, "duplicate or invalid metadata key");
            return -1;
        }
        yaml_event_t value;
        if (yaml_next(parse, &value) != 0)
            return -1;
        if (strcmp(key, "name") != 0) {
            yaml_event_delete(&value);
            yaml_fail(parse, "unknown metadata key: %s", key);
            return -1;
        }
        /* The name is not quoted in the reason: it is not a valid one. */
        char name[PROFILE_NAME_CAP];
        if (yaml_string(parse, &value, name, sizeof(name)) != 0 ||
            !profile_name_valid(name)) {
            parse->error[0] = '\0';
            yaml_fail(parse, "metadata.name is not a valid profile name");
            return -1;
        }
        snprintf(profile->name, sizeof(profile->name), "%s", name);
    }
}

/* The root of a profile definition: its four keys, in any order. */
static int parse_definition(struct yaml_parse *parse, yaml_event_t *event,
                            struct profile *profile)
{
    if (yaml_mapping_start(parse, event) != 0)
        return -1;
    char seen[PROFILE_SEEN_KEY_CAP][64] = {{0}};
    size_t count = 0;
    int have_version = 0, have_kind = 0, have_spec = 0;
    for (;;) {
        yaml_event_t key_event;
        if (yaml_next(parse, &key_event) != 0)
            return -1;
        if (key_event.type == YAML_MAPPING_END_EVENT) {
            yaml_event_delete(&key_event);
            break;
        }
        char key[64];
        if (yaml_string(parse, &key_event, key, sizeof(key)) != 0 ||
            seen_key(seen, &count, key) != 0) {
            yaml_fail(parse, "duplicate or invalid definition key");
            return -1;
        }
        yaml_event_t value;
        if (yaml_next(parse, &value) != 0)
            return -1;
        int rc;
        if (strcmp(key, "apiVersion") == 0) {
            rc = parse_constant(parse, &value, key,
                                PROFILE_DEFINITION_API_VERSION);
            have_version = 1;
        } else if (strcmp(key, "kind") == 0) {
            rc = parse_constant(parse, &value, key, PROFILE_DEFINITION_KIND);
            have_kind = 1;
        } else if (strcmp(key, "metadata") == 0) {
            rc = parse_metadata(parse, &value, profile);
        } else if (strcmp(key, "spec") == 0) {
            rc = parse_root(parse, &value, profile);
            have_spec = 1;
        } else {
            yaml_event_delete(&value);
            yaml_fail(parse, "unknown definition key: %s", key);
            return -1;
        }
        if (rc != 0)
            return -1;
    }
    if (!have_version || !have_kind || !have_spec || !profile->name[0]) {
        yaml_fail(parse, "a profile definition requires apiVersion, kind, "
                  "metadata.name and spec");
        return -1;
    }
    return 0;
}

typedef int (*profile_root_parser)(struct yaml_parse *parse,
                                   yaml_event_t *event,
                                   struct profile *profile);

/* Parses the one document of the parser's input into profile with root,
 * which reads the document's root node, and releases the parser. On failure
 * errno is EINVAL and reason, when given, says why. */
static int profile_parse_document(struct yaml_parse *parse,
                                  profile_root_parser root,
                                  struct profile *profile, char *reason)
{
    int rc = -1;
    yaml_event_t event;
    if (yaml_expect(parse, &event, YAML_STREAM_START_EVENT) != 0)
        goto out;
    yaml_event_delete(&event);
    if (yaml_expect(parse, &event, YAML_DOCUMENT_START_EVENT) != 0)
        goto out;
    if (event.data.document_start.tag_directives.start !=
        event.data.document_start.tag_directives.end) {
        yaml_fail(parse, "YAML tag directives are not supported");
        yaml_event_delete(&event);
        goto out;
    }
    yaml_event_delete(&event);
    if (yaml_next(parse, &event) != 0 || root(parse, &event, profile) != 0)
        goto out;
    if (yaml_expect(parse, &event, YAML_DOCUMENT_END_EVENT) != 0)
        goto out;
    yaml_event_delete(&event);
    if (yaml_expect(parse, &event, YAML_STREAM_END_EVENT) != 0)
        goto out;
    yaml_event_delete(&event);
    rc = 0;
out:
    if (rc != 0 && !parse->error[0])
        yaml_fail(parse, "expected exactly one YAML configuration document");
    yaml_parser_delete(&parse->parser);
    if (rc != 0) {
        reason_set(reason, parse->error);
        errno = EINVAL;
    }
    return rc;
}

/* The configuration in config.yaml, already open as file. On failure errno
 * is EINVAL, or ENOMEM without a parser. */
static int profile_parse_yaml(FILE *file, struct profile *profile,
                              char *reason)
{
    struct yaml_parse parse;
    memset(&parse, 0, sizeof(parse));
    if (!yaml_parser_initialize(&parse.parser)) {
        errno = ENOMEM;
        return -1;
    }
    yaml_parser_set_input_file(&parse.parser, file);
    return profile_parse_document(&parse, parse_root, profile, reason);
}

/* The document in length bytes of text, which stay valid during the call:
 * a configuration for parse_root, a profile definition for
 * parse_definition. */
static int profile_parse_text(const char *text, size_t length,
                              profile_root_parser root,
                              struct profile *profile, char *reason)
{
    struct yaml_parse parse;
    memset(&parse, 0, sizeof(parse));
    if (!yaml_parser_initialize(&parse.parser)) {
        errno = ENOMEM;
        return -1;
    }
    yaml_parser_set_input_string(&parse.parser, (const unsigned char *)text,
                                 length);
    return profile_parse_document(&parse, root, profile, reason);
}

int profile_definition_parse(const char *text, size_t length,
                             struct profile *profile,
                             char reason[PROFILE_REASON_CAP])
{
    reason[0] = '\0';
    if (!text || !profile || length > PROFILE_DEFINITION_CAP) {
        reason_set(reason, "a profile definition holds at most 65536 bytes");
        errno = EINVAL;
        return -1;
    }
    profile_defaults(profile);
    int rc = profile_parse_text(text, length, parse_definition, profile,
                                reason);
    if (rc != 0 && !reason[0]) {
        int saved = errno;
        reason_set(reason, strerror(saved));
        errno = saved;
    }
    return rc;
}

static int profile_open_config(const struct profile *profile, FILE **file_out,
                               char *reason)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, PROFILE_CONFIG_FILE, path, sizeof(path))) {
        errno = ENAMETOOLONG;
        return -1;
    }
    /* O_NONBLOCK: a FIFO in place of the file must not make the open wait for
     * a writer; it is refused below as not a regular file. */
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK);
    if (fd < 0)
        return errno == ENOENT ? 0 : -1;
    struct stat status;
    if (fstat(fd, &status) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    /* Not a failed call: errno still holds whatever an earlier one left, and
     * a leftover ENOENT would report this configuration as missing. */
    if (!S_ISREG(status.st_mode) || status.st_size > PROFILE_YAML_CAP) {
        reason_set(reason, S_ISREG(status.st_mode) ?
                   "config.yaml is larger than 65536 bytes" :
                   "config.yaml is not a regular file");
        close(fd);
        errno = EINVAL;
        return -1;
    }
    FILE *file = fdopen(fd, "r");
    if (!file) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    *file_out = file;
    return 1;
}

int profile_locate(struct profile *profile)
{
    char root[PROFILE_PATH_CAP];
    if (!profile || !profile_name_valid(profile->name) ||
        !hamn_home(root, sizeof(root))) {
        errno = EINVAL;
        return -1;
    }
    if (snprintf(profile->dir, sizeof(profile->dir), "%s/%s", root,
                 profile->name) >= (int)sizeof(profile->dir)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

/* The profile cannot be read (-1), or with create its directory cannot be
 * made (PROFILE_UNCREATED): there was no configuration to read yet. reason,
 * when given, receives the cause of a refusal that errno alone does not name;
 * it is left untouched otherwise. */
#define PROFILE_UNCREATED (-2)

static int profile_read(struct profile *profile, const char *name, int create,
                        char *reason)
{
    if (!profile || !profile_name_valid(name)) {
        reason_set(reason, "invalid profile name");
        errno = EINVAL;
        return -1;
    }
    profile_defaults(profile);
    snprintf(profile->name, sizeof(profile->name), "%s", name);
    if (profile_locate(profile) != 0) {
        if (errno == EINVAL)
            reason_set(reason, "~/.hamn is not a directory that only this "
                       "user can write");
        return -1;
    }
    if (create && fs_mkdirs(profile->dir, 0700) != 0)
        return PROFILE_UNCREATED;
    {
        struct stat status;
        if (lstat(profile->dir, &status) != 0)
            return -1;
        if (!S_ISDIR(status.st_mode) || status.st_uid != geteuid() || (status.st_mode & 0022)) {
            reason_set(reason, "the profile directory is not a directory "
                       "that only this user can write");
            errno = EINVAL;
            return -1;
        }
    }
    FILE *file = NULL;
    int opened = profile_open_config(profile, &file, reason);
    if (opened == 0) {
        if (!create)
            errno = ENOENT;
        return create ? 0 : -1;
    }
    if (opened < 0)
        return -1;
    int rc = profile_parse_yaml(file, profile, reason);
    int saved = errno;
    if (fclose(file) != 0 && rc == 0) {
        saved = errno;
        rc = -1;
    }
    if (rc != 0)
        errno = saved;
    return rc;
}

int profile_load(struct profile *profile, const char *name)
{
    return profile_read(profile, name, 1, NULL) == 0 ? 0 : -1;
}

int profile_read_existing(struct profile *profile, const char *name)
{
    return profile_read(profile, name, 0, NULL);
}

/* profile_read with a reason for every failure: the error text when the
 * reader named none. errno is kept. */
static int profile_read_reason(struct profile *profile, const char *name,
                               int create, char reason[PROFILE_REASON_CAP])
{
    reason[0] = '\0';
    int rc = profile_read(profile, name, create, reason);
    if (rc != 0 && !reason[0]) {
        int saved = errno;
        snprintf(reason, PROFILE_REASON_CAP, "%s", strerror(saved));
        errno = saved;
    }
    return rc;
}

int profile_read_existing_reason(struct profile *profile, const char *name,
                                 char reason[PROFILE_REASON_CAP])
{
    return profile_read_reason(profile, name, 0, reason);
}

/* The sentence that reports the result rc of profile_read_reason, which left
 * errno and reason. The name is quoted only when it is a profile name: any
 * other text is the caller's and can hold anything. errno is kept. */
static int profile_explain(int rc, const char *name, const char *reason,
                           char failure[PROFILE_FAILURE_CAP])
{
    int saved = errno;
    if (rc == 0)
        failure[0] = '\0';
    else if (!profile_name_valid(name))
        snprintf(failure, PROFILE_FAILURE_CAP, "invalid profile name");
    else if (rc == PROFILE_UNCREATED)
        snprintf(failure, PROFILE_FAILURE_CAP, "cannot create profile %s: %s",
                 name, reason);
    else if (saved == ENOENT)
        snprintf(failure, PROFILE_FAILURE_CAP, "profile %s does not exist",
                 name);
    else
        snprintf(failure, PROFILE_FAILURE_CAP, PROFILE_UNREADABLE_FORMAT, name,
                 reason);
    errno = saved;
    return rc == 0 ? 0 : -1;
}

int profile_read_existing_explained(struct profile *profile, const char *name,
                                    char failure[PROFILE_FAILURE_CAP])
{
    char reason[PROFILE_REASON_CAP];
    int rc = profile_read_reason(profile, name, 0, reason);
    return profile_explain(rc, name, reason, failure);
}

int profile_load_explained(struct profile *profile, const char *name,
                           char failure[PROFILE_FAILURE_CAP])
{
    char reason[PROFILE_REASON_CAP];
    int rc = profile_read_reason(profile, name, 1, reason);
    return profile_explain(rc, name, reason, failure);
}

const char *profile_setting_key(enum profile_setting setting)
{
    static const char *const keys[PROFILE_SETTING_COUNT] = {
        [PROFILE_SETTING_CPUS] = "cpus",
        [PROFILE_SETTING_MEMORY_MIB] = "memoryMiB",
        [PROFILE_SETTING_DISK_GIB] = "diskGiB",
        [PROFILE_SETTING_MOUNT_HOME] = "mountHome",
        [PROFILE_SETTING_HOME_READ_ONLY] = "homeReadOnly",
        [PROFILE_SETTING_MOUNT_INOTIFY] = "mountInotify",
        [PROFILE_SETTING_DOCKER_DAEMON_JSON] = "docker.daemonJson",
        [PROFILE_SETTING_ROSETTA] = "rosetta",
        [PROFILE_SETTING_NESTED_VIRTUALIZATION] = "nestedVirtualization",
        [PROFILE_SETTING_SSH_AGENT] = "sshAgent",
        [PROFILE_SETTING_MOUNTS] = "mounts",
        [PROFILE_SETTING_PROVISION] = "provision",
    };
    return (unsigned)setting < PROFILE_SETTING_COUNT ? keys[setting] : NULL;
}

static int mounts_differ(const struct profile *a, const struct profile *b)
{
    if (a->mount_count != b->mount_count)
        return 1;
    for (size_t index = 0; index < a->mount_count; index++) {
        const struct profile_mount *left = &a->mounts[index];
        const struct profile_mount *right = &b->mounts[index];
        if (strcmp(left->location, right->location) != 0 ||
            strcmp(left->mount_point, right->mount_point) != 0 ||
            !left->writable != !right->writable)
            return 1;
    }
    return 0;
}

static int hooks_differ(const struct profile *a, const struct profile *b)
{
    if (a->hook_count != b->hook_count)
        return 1;
    for (size_t index = 0; index < a->hook_count; index++) {
        const struct profile_hook *left = &a->hooks[index];
        const struct profile_hook *right = &b->hooks[index];
        if (strcmp(left->stage, right->stage) != 0 ||
            strcmp(left->command, right->command) != 0 ||
            left->timeout_seconds != right->timeout_seconds ||
            !left->warn != !right->warn)
            return 1;
    }
    return 0;
}

unsigned profile_diff(const struct profile *a, const struct profile *b)
{
    const int differs[PROFILE_SETTING_COUNT] = {
        [PROFILE_SETTING_CPUS] = a->cpus != b->cpus,
        [PROFILE_SETTING_MEMORY_MIB] = a->mem_mib != b->mem_mib,
        [PROFILE_SETTING_DISK_GIB] = a->disk_gib != b->disk_gib,
        [PROFILE_SETTING_MOUNT_HOME] = !a->mount_home != !b->mount_home,
        [PROFILE_SETTING_HOME_READ_ONLY] =
            !a->home_read_only != !b->home_read_only,
        [PROFILE_SETTING_MOUNT_INOTIFY] =
            !a->mount_inotify != !b->mount_inotify,
        [PROFILE_SETTING_DOCKER_DAEMON_JSON] =
            strcmp(a->docker_daemon_json, b->docker_daemon_json) != 0,
        [PROFILE_SETTING_ROSETTA] = !a->rosetta != !b->rosetta,
        [PROFILE_SETTING_NESTED_VIRTUALIZATION] =
            !a->nested_virtualization != !b->nested_virtualization,
        [PROFILE_SETTING_SSH_AGENT] = !a->ssh_agent != !b->ssh_agent,
        [PROFILE_SETTING_MOUNTS] = mounts_differ(a, b),
        [PROFILE_SETTING_PROVISION] = hooks_differ(a, b),
    };
    unsigned settings = 0;
    for (int setting = 0; setting < PROFILE_SETTING_COUNT; setting++) {
        if (differs[setting])
            settings |= 1u << setting;
    }
    return settings;
}

static int text_append(struct yaml_text *text, const char *format, ...)
{
    if (text->length >= sizeof(text->data))
        return -1;
    va_list arguments;
    va_start(arguments, format);
    int written = vsnprintf(text->data + text->length,
                            sizeof(text->data) - text->length,
                            format, arguments);
    va_end(arguments);
    if (written < 0 || (size_t)written >= sizeof(text->data) - text->length)
        return -1;
    text->length += (size_t)written;
    return 0;
}

static int text_quote(struct yaml_text *text, const char *value)
{
    if (text_append(text, "\"") != 0)
        return -1;
    for (const unsigned char *cursor = (const unsigned char *)value; *cursor;
         cursor++) {
        switch (*cursor) {
        case '\\': if (text_append(text, "\\\\") != 0) return -1; break;
        case '\"': if (text_append(text, "\\\"") != 0) return -1; break;
        case '\n': if (text_append(text, "\\n") != 0) return -1; break;
        case '\r': if (text_append(text, "\\r") != 0) return -1; break;
        case '\t': if (text_append(text, "\\t") != 0) return -1; break;
        default:
            if (*cursor < 0x20) {
                text->unwritable = 1;
                return -1;
            }
            if (text_append(text, "%c", *cursor) != 0)
                return -1;
        }
    }
    return text_append(text, "\"");
}

static int profile_serialize(const struct profile *profile,
                             struct yaml_text *text)
{
    if (profile_validate(profile) != 0)
        return -1;
    memset(text, 0, sizeof(*text));
    if (text_append(text, "cpus: %u\nmemoryMiB: %u\ndiskGiB: %u\n",
                    profile->cpus, profile->mem_mib, profile->disk_gib) != 0 ||
        text_append(text, "mountHome: %s\nhomeReadOnly: %s\nmountInotify: %s\n",
                    profile->mount_home ? "true" : "false",
                    profile->home_read_only ? "true" : "false",
                    profile->mount_inotify ? "true" : "false") != 0)
        return -1;
    if (text_append(text, "docker:\n  daemonJson: ") != 0 ||
        text_quote(text, profile->docker_daemon_json) != 0)
        return -1;
    if (text_append(text,
                    "\nrosetta: %s\nnestedVirtualization: %s\nsshAgent: %s\n",
                    profile->rosetta ? "true" : "false",
                    profile->nested_virtualization ? "true" : "false",
                    profile->ssh_agent ? "true" : "false") != 0)
        return -1;
    if (profile->mount_count == 0) {
        if (text_append(text, "mounts: []\n") != 0)
            return -1;
    } else if (text_append(text, "mounts:\n") != 0) {
        return -1;
    }
    for (size_t index = 0; index < profile->mount_count; index++) {
        const struct profile_mount *mount = &profile->mounts[index];
        if (text_append(text, "  - location: ") != 0 ||
            text_quote(text, mount->location) != 0 ||
            text_append(text, "\n    mountPoint: ") != 0 ||
            text_quote(text, mount->mount_point) != 0 ||
            text_append(text, "\n    writable: %s\n",
                        mount->writable ? "true" : "false") != 0)
            return -1;
    }
    if (profile->hook_count == 0) {
        if (text_append(text, "provision: []\n") != 0)
            return -1;
    } else if (text_append(text, "provision:\n") != 0) {
        return -1;
    }
    for (size_t index = 0; index < profile->hook_count; index++) {
        const struct profile_hook *hook = &profile->hooks[index];
        if (text_append(text, "  - stage: ") != 0 ||
            text_quote(text, hook->stage) != 0 ||
            text_append(text, "\n    command: ") != 0 ||
            text_quote(text, hook->command) != 0 ||
            text_append(text, "\n    timeoutSeconds: %u\n    mode: %s\n",
                        hook->timeout_seconds, hook->warn ? "warn" : "fail") != 0)
            return -1;
    }
    return 0;
}

/* Whether text, the serialized form of profile, reads as the same settings.
 * text_quote writes every byte from 0x7f up as it is, and the YAML reader
 * refuses some of those characters (DEL, the C1 controls) and treats others
 * as a line break, which it folds together with the blanks around it. */
static int profile_text_reads_back(const struct profile *profile,
                                   const struct yaml_text *text)
{
    struct profile stored;
    profile_defaults(&stored);
    return profile_parse_text(text->data, text->length, parse_root, &stored,
                              NULL) == 0 &&
           profile_diff(profile, &stored) == 0;
}

/* The text that profile_save writes for profile. On -1 nothing may be
 * written and errno says why: EINVAL for settings that break the schema,
 * EOVERFLOW for text beyond 64 KiB, EILSEQ for a string with a character
 * that config.yaml cannot carry. */
static int profile_text(const struct profile *profile, struct yaml_text *text)
{
    if (!profile || profile_validate(profile) != 0) {
        errno = EINVAL;
        return -1;
    }
    if (profile_serialize(profile, text) != 0) {
        errno = text->unwritable ? EILSEQ : EOVERFLOW;
        return -1;
    }
    if (!profile_text_reads_back(profile, text)) {
        errno = EILSEQ;
        return -1;
    }
    return 0;
}

int profile_storable(const struct profile *profile)
{
    struct yaml_text text;
    return profile_text(profile, &text) == 0;
}

int profile_save(const struct profile *profile)
{
    struct yaml_text text;
    if (profile_text(profile, &text) != 0)
        return -1;
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, PROFILE_CONFIG_FILE, path, sizeof(path))) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return fs_write_file_atomic(path, text.data, text.length, 0600);
}
