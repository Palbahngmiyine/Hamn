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
#define PROFILE_LEGACY_CONFIG_FILE "hamn.conf"
#define PROFILE_YAML_CAP (64 * 1024)
#define PROFILE_SEEN_KEY_CAP 16

struct yaml_parse {
    yaml_parser_t parser;
    char error[256];
};

struct yaml_text {
    char data[PROFILE_YAML_CAP];
    size_t length;
};

static void profile_defaults(struct profile *profile)
{
    memset(profile, 0, sizeof(*profile));
    profile->cpus = 4;
    profile->mem_mib = 4096;
    profile->disk_gib = 60;
    profile->mount_home = 1;
    snprintf(profile->kubernetes_version, sizeof(profile->kubernetes_version),
             "v1.36.2+k3s1");
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

int profile_resolve_name(const char *flag_name, const char *positional_name,
                         char out[PROFILE_NAME_CAP])
{
    const char *selected = flag_name && flag_name[0] ? flag_name :
        positional_name && positional_name[0] ? positional_name :
        getenv("HAMN_PROFILE");
    if (!selected || !selected[0])
        selected = "default";
    if (!profile_name_valid(selected)) {
        errno = EINVAL;
        return -1;
    }
    snprintf(out, PROFILE_NAME_CAP, "%s", selected);
    return 0;
}

const char *hamn_home(char *buf, size_t cap)
{
    const char *home = getenv("HOME");
    if (!home || !home[0] || snprintf(buf, cap, "%s/.hamn", home) >=
        (int)cap)
        return NULL;
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

int profile_docker_context_name(const struct profile *profile, char *out,
                                size_t cap)
{
    if (!profile || !profile_name_valid(profile->name) || !out || cap == 0) {
        errno = EINVAL;
        return -1;
    }
    int length = strcmp(profile->name, "default") == 0 ?
        snprintf(out, cap, "hamn") : snprintf(out, cap, "hamn-%s",
                                                profile->name);
    if (length < 0 || length >= (int)cap) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

static void yaml_fail(struct yaml_parse *parse, const char *format, ...)
{
    if (parse->error[0])
        return;
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(parse->error, sizeof(parse->error), format, arguments);
    va_end(arguments);
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

static int profile_validate(const struct profile *profile)
{
    if (!profile || !profile->cpus || !profile->mem_mib || !profile->disk_gib ||
        (profile->home_read_only && !profile->mount_home) ||
        strcmp(profile->kubernetes_version, "v1.36.2+k3s1") != 0 ||
        !profile_docker_daemon_json_valid(profile->docker_daemon_json) ||
        profile->mount_count > PROFILE_MAX_MOUNTS ||
        profile->hook_count > PROFILE_MAX_HOOKS)
        return -1;
    int has_writable_share = profile->mount_home && !profile->home_read_only;
    for (size_t index = 0; index < profile->mount_count; index++) {
        const struct profile_mount *mount = &profile->mounts[index];
        if (!clean_absolute_path(mount->location, 1) ||
            !clean_absolute_path(mount->mount_point, 0))
            return -1;
        if (mount->writable)
            has_writable_share = 1;
        for (size_t other = 0; other < index; other++) {
            if (strcmp(mount->mount_point,
                       profile->mounts[other].mount_point) == 0)
                return -1;
        }
    }
    if (profile->mount_inotify && !has_writable_share)
        return -1;
    for (size_t index = 0; index < profile->hook_count; index++) {
        const struct profile_hook *hook = &profile->hooks[index];
        if (!hook_stage_valid(hook->stage) || !hook->command[0] ||
            hook->timeout_seconds == 0 || hook->timeout_seconds > 3600)
            return -1;
    }
    return 0;
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

static int parse_kubernetes(struct yaml_parse *parse, yaml_event_t *event,
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
            yaml_fail(parse, "duplicate or invalid kubernetes key");
            return -1;
        }
        yaml_event_t value;
        if (yaml_next(parse, &value) != 0)
            return -1;
        int rc;
        if (strcmp(key, "enabled") == 0)
            rc = yaml_bool(parse, &value, &profile->kubernetes_enabled);
        else if (strcmp(key, "version") == 0)
            rc = yaml_string(parse, &value, profile->kubernetes_version,
                             sizeof(profile->kubernetes_version));
        else {
            yaml_event_delete(&value);
            yaml_fail(parse, "unknown kubernetes key: %s", key);
            return -1;
        }
        if (rc != 0)
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
            if (rc == 0 && strcmp(mode, "fail") != 0 &&
                strcmp(mode, "warn") != 0) {
                yaml_fail(parse, "provision mode must be fail or warn");
                return -1;
            }
            hook->warn = strcmp(mode, "warn") == 0;
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
        else if (strcmp(key, "kubernetes") == 0)
            rc = parse_kubernetes(parse, &value, profile);
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
    if (profile_validate(profile) != 0) {
        yaml_fail(parse, "configuration violates the Hamn profile schema");
        return -1;
    }
    return 0;
}

static int profile_parse_yaml(FILE *file, struct profile *profile)
{
    struct yaml_parse parse;
    memset(&parse, 0, sizeof(parse));
    if (!yaml_parser_initialize(&parse.parser)) {
        errno = ENOMEM;
        return -1;
    }
    yaml_parser_set_input_file(&parse.parser, file);
    int rc = -1;
    yaml_event_t event;
    if (yaml_next(&parse, &event) != 0 || event.type != YAML_STREAM_START_EVENT)
        goto out;
    yaml_event_delete(&event);
    if (yaml_next(&parse, &event) != 0 ||
        event.type != YAML_DOCUMENT_START_EVENT)
        goto out;
    if (event.data.document_start.tag_directives.start !=
        event.data.document_start.tag_directives.end) {
        yaml_fail(&parse, "YAML tag directives are not supported");
        yaml_event_delete(&event);
        goto out;
    }
    yaml_event_delete(&event);
    if (yaml_next(&parse, &event) != 0 || parse_root(&parse, &event, profile) != 0)
        goto out;
    if (yaml_next(&parse, &event) != 0 || event.type != YAML_DOCUMENT_END_EVENT)
        goto out;
    yaml_event_delete(&event);
    if (yaml_next(&parse, &event) != 0 || event.type != YAML_STREAM_END_EVENT)
        goto out;
    yaml_event_delete(&event);
    rc = 0;
out:
    if (rc != 0 && !parse.error[0])
        yaml_fail(&parse, "expected exactly one YAML configuration document");
    yaml_parser_delete(&parse.parser);
    if (rc != 0)
        errno = EINVAL;
    return rc;
}

static int profile_legacy_config_state(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, PROFILE_LEGACY_CONFIG_FILE, path, sizeof(path)))
        return -1;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return errno == ENOENT ? 0 : -1;
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_size > 16384) {
        int saved = errno ? errno : EINVAL;
        close(fd);
        errno = saved;
        return -1;
    }
    char text[16385];
    ssize_t count = read(fd, text, sizeof(text) - 1);
    int saved = errno;
    if (close(fd) != 0 && count >= 0)
        return -1;
    if (count < 0) {
        errno = saved;
        return -1;
    }
    text[count] = '\0';
    char *line = text;
    while (line && *line) {
        char *end = strchr(line, '\n');
        if (end)
            *end = '\0';
        if (strcmp(line, "runtime=containerd") == 0 ||
            strcmp(line, "runtime=hamn") == 0)
            return 1;
        line = end ? end + 1 : NULL;
    }
    errno = EINVAL;
    return -1;
}

static int profile_open_config(const struct profile *profile, FILE **file_out)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, PROFILE_CONFIG_FILE, path, sizeof(path)))
        return -1;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return errno == ENOENT ? 0 : -1;
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_size > PROFILE_YAML_CAP) {
        int saved = errno ? errno : EINVAL;
        close(fd);
        errno = saved;
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

int profile_load(struct profile *profile, const char *name)
{
    if (!profile || !profile_name_valid(name)) {
        errno = EINVAL;
        return -1;
    }
    profile_defaults(profile);
    char root[PROFILE_PATH_CAP];
    if (!hamn_home(root, sizeof(root))) {
        errno = EINVAL;
        return -1;
    }
    snprintf(profile->name, sizeof(profile->name), "%s", name);
    if (snprintf(profile->dir, sizeof(profile->dir), "%s/%s", root, name) >=
        (int)sizeof(profile->dir)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    if (fs_mkdirs(profile->dir, 0700) != 0)
        return -1;
    int legacy = profile_legacy_config_state(profile);
    if (legacy == 1) {
        errno = EPROTONOSUPPORT;
        return -1;
    }
    if (legacy < 0 && errno != ENOENT)
        return -1;
    FILE *file = NULL;
    int opened = profile_open_config(profile, &file);
    if (opened == 0)
        return 0;
    if (opened < 0)
        return -1;
    int rc = profile_parse_yaml(file, profile);
    int saved = errno;
    if (fclose(file) != 0 && rc == 0)
        rc = -1;
    if (rc != 0)
        errno = saved ? saved : EINVAL;
    return rc;
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
            if (*cursor < 0x20 || text_append(text, "%c", *cursor) != 0)
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
        text_quote(text, profile->docker_daemon_json) != 0 ||
        text_append(text, "\nkubernetes:\n  enabled: %s\n  version: ",
                    profile->kubernetes_enabled ? "true" : "false") != 0 ||
        text_quote(text, profile->kubernetes_version) != 0 ||
        text_append(text,
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

int profile_save(const struct profile *profile)
{
    if (!profile || profile_validate(profile) != 0) {
        errno = EINVAL;
        return -1;
    }
    int legacy = profile_legacy_config_state(profile);
    if (legacy == 1) {
        errno = EPROTONOSUPPORT;
        return -1;
    }
    if (legacy < 0 && errno != ENOENT)
        return -1;
    struct yaml_text text;
    if (profile_serialize(profile, &text) != 0) {
        errno = EOVERFLOW;
        return -1;
    }
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, PROFILE_CONFIG_FILE, path, sizeof(path))) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return fs_write_file_atomic(path, text.data, text.length, 0600);
}

int profile_template_print(FILE *out)
{
    if (!out) {
        errno = EINVAL;
        return -1;
    }
    struct profile profile;
    profile_defaults(&profile);
    struct yaml_text text;
    if (profile_serialize(&profile, &text) != 0 ||
        fwrite(text.data, 1, text.length, out) != text.length || fflush(out) != 0)
        return -1;
    return 0;
}
