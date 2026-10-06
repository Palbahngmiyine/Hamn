#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "cli.h"
#include "cjson/cJSON.h"
#include "core/log.h"
#include "core/control.h"
#include "core/profile.h"
#include "util/fs.h"

#define DIAGNOSTIC_LOG_TAIL_BYTES (128U * 1024U)

struct text_buffer {
    char *data;
    size_t length;
    size_t capacity;
};

struct tar_header {
    char name[100];
    char mode[8];
    char uid[8];
    char gid[8];
    char size[12];
    char mtime[12];
    char checksum[8];
    char typeflag;
    char linkname[100];
    char magic[6];
    char version[2];
    char uname[32];
    char gname[32];
    char devmajor[8];
    char devminor[8];
    char prefix[155];
    char padding[12];
};

_Static_assert(sizeof(struct tar_header) == 512,
               "ustar header must be exactly 512 bytes");

static void buffer_free(struct text_buffer *buffer)
{
    free(buffer->data);
    memset(buffer, 0, sizeof(*buffer));
}

static int buffer_reserve(struct text_buffer *buffer, size_t extra)
{
    if (extra > SIZE_MAX - buffer->length - 1) {
        errno = EOVERFLOW;
        return -1;
    }
    size_t needed = buffer->length + extra + 1;
    if (needed <= buffer->capacity)
        return 0;

    size_t capacity = buffer->capacity ? buffer->capacity : 4096;
    while (capacity < needed) {
        if (capacity > SIZE_MAX / 2) {
            capacity = needed;
            break;
        }
        capacity *= 2;
    }
    char *grown = realloc(buffer->data, capacity);
    if (!grown)
        return -1;
    buffer->data = grown;
    buffer->capacity = capacity;
    return 0;
}

static int buffer_append(struct text_buffer *buffer, const char *data,
                         size_t length)
{
    if (buffer_reserve(buffer, length) != 0)
        return -1;
    memcpy(buffer->data + buffer->length, data, length);
    buffer->length += length;
    buffer->data[buffer->length] = '\0';
    return 0;
}

static int contains_ci(const char *text, const char *needle)
{
    size_t needle_length = strlen(needle);
    if (!needle_length)
        return 1;
    for (; *text; text++) {
        size_t i = 0;
        while (i < needle_length && text[i] &&
               tolower((unsigned char)text[i]) ==
                   tolower((unsigned char)needle[i]))
            i++;
        if (i == needle_length)
            return 1;
    }
    return 0;
}

static int line_starts_field(const char *line, const char *field)
{
    while (*line == ' ' || *line == '\t' || *line == '-')
        line++;
    int quoted = *line == '"';
    if (quoted)
        line++;
    size_t length = strlen(field);
    if (strncasecmp(line, field, length) != 0)
        return 0;
    line += length;
    if (quoted) {
        if (*line != '"')
            return 0;
        line++;
    }
    while (*line == ' ' || *line == '\t')
        line++;
    return *line == ':';
}

static int line_starts_sensitive_section(const char *line)
{
    static const char *const fields[] = {
        "data", "stringData", "binaryData", "env", "envFrom", "args",
        "command", "clusters", "users", "token", "accessToken",
        "access_token", "refreshToken", "refresh_token", "secret",
        "password", "passwd", "credential", "client-key", "client_key",
        "client-certificate-data", "certificate-authority-data",
        "authorization", "cookie"
    };
    for (size_t i = 0; i < sizeof(fields) / sizeof(fields[0]); i++) {
        if (line_starts_field(line, fields[i]))
            return 1;
    }
    return 0;
}

static int contains_high_entropy_value(const char *line)
{
    const char *p = line;
    while (*p) {
        while (*p && !(isalnum((unsigned char)*p) || *p == '_' ||
                       *p == '-' || *p == '+' || *p == '/' || *p == '=' ||
                       *p == '.'))
            p++;
        const char *start = p;
        int lower = 0, upper = 0, digit = 0;
        int hexadecimal = 1;
        int opaque_encoding = 1;
        while (*p && (isalnum((unsigned char)*p) || *p == '_' ||
                      *p == '-' || *p == '+' || *p == '/' || *p == '=' ||
                      *p == '.')) {
            lower |= islower((unsigned char)*p) != 0;
            upper |= isupper((unsigned char)*p) != 0;
            digit |= isdigit((unsigned char)*p) != 0;
            hexadecimal &= isxdigit((unsigned char)*p) != 0;
            opaque_encoding &= isalnum((unsigned char)*p) || *p == '_' ||
                *p == '-' || *p == '+' || *p == '/' || *p == '=';
            p++;
        }
        size_t token_length = (size_t)(p - start);
        if ((token_length >= 32 && lower && upper && digit) ||
            (token_length >= 32 && hexadecimal) ||
            (token_length >= 32 && opaque_encoding))
            return 1;
    }
    return 0;
}

static int bootstrap_token_character(unsigned char c)
{
    return (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9');
}

static int contains_kubernetes_bootstrap_token(const char *line)
{
    enum { token_id_length = 6, token_secret_length = 16 };
    const size_t token_length = token_id_length + 1 + token_secret_length;
    size_t line_length = strlen(line);

    for (size_t offset = 0; offset + token_length <= line_length; offset++) {
        if (offset > 0 &&
            bootstrap_token_character((unsigned char)line[offset - 1]))
            continue;
        size_t i = 0;
        while (i < token_id_length && bootstrap_token_character(
                   (unsigned char)line[offset + i]))
            i++;
        if (i != token_id_length || line[offset + token_id_length] != '.')
            continue;
        i = 0;
        while (i < token_secret_length && bootstrap_token_character(
                   (unsigned char)line[offset + token_id_length + 1 + i]))
            i++;
        if (i != token_secret_length)
            continue;
        size_t end = offset + token_length;
        if (end < line_length &&
            bootstrap_token_character((unsigned char)line[end]))
            continue;
        return 1;
    }
    return 0;
}

static int line_has_sensitive_marker(const char *line)
{
    static const char *const markers[] = {
        "authorization", "bearer", "token", "secret", "password",
        "passwd", "credential", "client-key", "client_key",
        "client-certificate-data", "certificate-authority-data",
        "private key", "access_key", "access-key", "api_key", "apikey",
        "cookie", "kubeconfig", "current-context", "-----begin",
        "aws_secret_access_key", "akia", "asia"
    };
    for (size_t i = 0; i < sizeof(markers) / sizeof(markers[0]); i++) {
        if (contains_ci(line, markers[i]))
            return 1;
    }
    if (strstr(line, "://") && strchr(strstr(line, "://") + 3, '@'))
        return 1;
    if (contains_ci(line, "eyj") && strchr(line, '.'))
        return 1;
    return contains_kubernetes_bootstrap_token(line) ||
        contains_high_entropy_value(line);
}

static int append_redacted_line(struct text_buffer *output)
{
    static const char replacement[] = "[REDACTED sensitive log line]\n";
    return buffer_append(output, replacement, sizeof(replacement) - 1);
}

static int redact_log_text(const char *text, size_t length,
                           struct text_buffer *output)
{
    size_t offset = 0;
    int sensitive_section_indent = -1;
    int private_key_block = 0;

    while (offset < length) {
        size_t end = offset;
        while (end < length && text[end] != '\n')
            end++;
        size_t line_length = end - offset;
        char *line = calloc(line_length + 1, 1);
        if (!line)
            return -1;
        size_t cleaned_length = 0;
        for (size_t i = 0; i < line_length; i++) {
            unsigned char c = (unsigned char)text[offset + i];
            if (c == 0x1b && i + 1 < line_length && text[offset + i + 1] ==
                    '[') {
                i += 2;
                while (i < line_length) {
                    unsigned char sequence =
                        (unsigned char)text[offset + i];
                    if (sequence >= 0x40 && sequence <= 0x7e)
                        break;
                    i++;
                }
                continue;
            }
            if (c == '\0' || (c < 0x20 && c != '\t' && c != '\r'))
                continue;
            line[cleaned_length++] = (char)c;
        }
        line_length = cleaned_length;
        size_t indentation = 0;
        while (indentation < line_length &&
               (line[indentation] == ' ' || line[indentation] == '\t'))
            indentation++;
        int has_content = indentation < line_length;
        if (sensitive_section_indent >= 0 && has_content &&
            indentation <= (size_t)sensitive_section_indent)
            sensitive_section_indent = -1;

        int private_key_start = contains_ci(line, "-----begin") &&
            contains_ci(line, "private key-----");
        int private_key_end = contains_ci(line, "-----end") &&
            contains_ci(line, "private key-----");
        if (private_key_start)
            private_key_block = 1;
        int section_start = line_starts_sensitive_section(line);
        if (section_start)
            sensitive_section_indent = (int)indentation;
        int sensitive_marker = line_has_sensitive_marker(line);
        int sensitive = private_key_block ||
            sensitive_section_indent >= 0 || sensitive_marker ||
            (contains_ci(line, "kind") && contains_ci(line, "secret"));

        int rc = sensitive ? append_redacted_line(output) :
            buffer_append(output, line, line_length);
        if (rc == 0 && !sensitive)
            rc = buffer_append(output, "\n", 1);
        free(line);
        if (rc != 0)
            return -1;
        if (private_key_end)
            private_key_block = 0;
        offset = end < length ? end + 1 : end;
    }
    if (!output->length)
        return buffer_append(output, "(empty log)\n", 12);
    return 0;
}

static int open_profile_log(const struct profile *profile, const char *name)
{
    int profile_fd = open(profile->dir,
                          O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_DIRECTORY);
    if (profile_fd < 0)
        return -1;
    int logs_fd = openat(profile_fd, "logs",
                         O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_DIRECTORY);
    close(profile_fd);
    if (logs_fd < 0)
        return -1;
    int fd = openat(logs_fd, name, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    close(logs_fd);
    return fd;
}

static int read_log_tail(const struct profile *profile, const char *name,
                         struct text_buffer *output)
{
    int fd = open_profile_log(profile, name);
    if (fd < 0)
        return buffer_append(output, "(log unavailable)\n", 18);

    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size < 0) {
        close(fd);
        return buffer_append(output, "(log unavailable)\n", 18);
    }

    off_t offset = st.st_size > (off_t)DIAGNOSTIC_LOG_TAIL_BYTES ?
        st.st_size - (off_t)DIAGNOSTIC_LOG_TAIL_BYTES : 0;
    size_t wanted = (size_t)(st.st_size - offset);
    char *raw = malloc(wanted + 1);
    if (!raw) {
        close(fd);
        return -1;
    }
    if (lseek(fd, offset, SEEK_SET) < 0) {
        free(raw);
        close(fd);
        return buffer_append(output, "(log unavailable)\n", 18);
    }

    size_t used = 0;
    while (used < wanted) {
        ssize_t count = read(fd, raw + used, wanted - used);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            free(raw);
            close(fd);
            return buffer_append(output, "(log unavailable)\n", 18);
        }
        if (count == 0)
            break;
        used += (size_t)count;
    }
    close(fd);
    raw[used] = '\0';

    const char *start = raw;
    size_t safe_length = used;
    if (offset > 0) {
        char *newline = memchr(raw, '\n', used);
        if (!newline) {
            free(raw);
            return buffer_append(output,
                                 "[REDACTED truncated log line]\n", 30);
        }
        start = newline + 1;
        safe_length = used - (size_t)(start - raw);
    }
    int rc = redact_log_text(start, safe_length, output);
    free(raw);
    return rc;
}

/* The status that `vm status` reports for this profile, so the archive holds
 * the same answer and not a second derivation. The caller deletes it. */
static cJSON *query_snapshot(const struct profile *profile)
{
    char *text = NULL;
    if (hamn_control_query(profile->name, &text) != 0)
        return NULL;
    cJSON *snapshot = cJSON_Parse(text);
    hamn_control_free(text);
    return snapshot;
}

static char *build_status_json(const struct profile *profile,
                               const cJSON *snapshot)
{
    const cJSON *state = cJSON_GetObjectItemCaseSensitive(snapshot, "state");
    const cJSON *docker_status =
        cJSON_GetObjectItemCaseSensitive(snapshot, "dockerStatus");
    const cJSON *host_free =
        cJSON_GetObjectItemCaseSensitive(snapshot, "hostFreeMiB");
    if (!cJSON_IsString(state) || !cJSON_IsString(docker_status) ||
        !(cJSON_IsNumber(host_free) || cJSON_IsNull(host_free)))
        return NULL;

    cJSON *root = cJSON_CreateObject();
    cJSON *vm = NULL, *logs = NULL;
    cJSON *free_space = cJSON_Duplicate(host_free, 0);
    if (!root || !free_space ||
        !cJSON_AddNumberToObject(root, "schemaVersion", 1) ||
        !cJSON_AddStringToObject(root, "hamnVersion", HAMN_VERSION) ||
        !cJSON_AddStringToObject(root, "profile", profile->name) ||
        !(vm = cJSON_AddObjectToObject(root, "vm")) ||
        !cJSON_AddStringToObject(vm, "state", state->valuestring) ||
        !cJSON_AddNumberToObject(vm, "cpus", profile->cpus) ||
        !cJSON_AddNumberToObject(vm, "memoryMiB", profile->mem_mib) ||
        !cJSON_AddNumberToObject(vm, "diskGiB", profile->disk_gib) ||
        !cJSON_AddStringToObject(vm, "dockerStatus",
                                 docker_status->valuestring) ||
        !cJSON_AddItemToObject(root, "hostFreeMiB", free_space) ||
        !(logs = cJSON_AddObjectToObject(root, "logs")) ||
        !cJSON_AddNumberToObject(logs, "tailBytes",
                                DIAGNOSTIC_LOG_TAIL_BYTES) ||
        !cJSON_AddBoolToObject(logs, "redacted", 1)) {
        if (free_space && !cJSON_GetObjectItemCaseSensitive(root, "hostFreeMiB"))
            cJSON_Delete(free_space);
        cJSON_Delete(root);
        return NULL;
    }
    char *text = cJSON_PrintUnformatted(root);
    cJSON_Delete(root);
    return text;
}

/* The last operation record as `vm status` reports it (`null` when there is
 * none). Its error text is free-form, so it is redacted like a log line. */
static char *build_operation_json(const cJSON *snapshot)
{
    const cJSON *recorded =
        cJSON_GetObjectItemCaseSensitive(snapshot, "lastOperation");
    cJSON *operation = recorded ? cJSON_Duplicate(recorded, 1) :
        cJSON_CreateNull();
    if (!operation)
        return NULL;
    const cJSON *error = cJSON_GetObjectItemCaseSensitive(operation, "error");
    if (cJSON_IsString(error) && error->valuestring[0]) {
        struct text_buffer redacted = { 0 };
        cJSON *replacement = NULL;
        if (redact_log_text(error->valuestring, strlen(error->valuestring),
                            &redacted) == 0 && redacted.data) {
            while (redacted.length > 0 &&
                   redacted.data[redacted.length - 1] == '\n')
                redacted.data[--redacted.length] = '\0';
            replacement = cJSON_CreateString(redacted.data);
        }
        buffer_free(&redacted);
        if (!replacement ||
            !cJSON_ReplaceItemInObjectCaseSensitive(operation, "error",
                                                    replacement)) {
            cJSON_Delete(replacement);
            cJSON_Delete(operation);
            return NULL;
        }
    }
    char *text = cJSON_PrintUnformatted(operation);
    cJSON_Delete(operation);
    return text;
}

struct archive_member {
    const char *name;
    const char *data;
    size_t length;
};

static char *build_manifest_json(const struct archive_member members[],
                                 size_t count)
{
    cJSON *root = cJSON_CreateObject();
    cJSON *files = NULL;
    if (!root || !cJSON_AddNumberToObject(root, "schemaVersion", 1) ||
        !cJSON_AddStringToObject(root, "format", "ustar") ||
        !cJSON_AddBoolToObject(root, "redacted", 1) ||
        !cJSON_AddStringToObject(root, "collectionPolicy",
                                "allowlisted metadata and bounded log tails") ||
        !(files = cJSON_AddArrayToObject(root, "files"))) {
        cJSON_Delete(root);
        return NULL;
    }
    for (size_t index = 0; index < count; index++) {
        cJSON *name = cJSON_CreateString(members[index].name);
        if (!name || !cJSON_AddItemToArray(files, name)) {
            cJSON_Delete(name);
            cJSON_Delete(root);
            return NULL;
        }
    }
    char *text = cJSON_PrintUnformatted(root);
    cJSON_Delete(root);
    return text;
}

static int full_write(int fd, const void *data, size_t length)
{
    const char *cursor = data;
    while (length > 0) {
        ssize_t count = write(fd, cursor, length);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (count == 0) {
            errno = EIO;
            return -1;
        }
        cursor += count;
        length -= (size_t)count;
    }
    return 0;
}

static int tar_add_file(int fd, const char *name, const char *data,
                        size_t length, time_t timestamp)
{
    if (!name || strlen(name) >= sizeof(((struct tar_header *)0)->name) ||
        length > 077777777777ULL) {
        errno = EOVERFLOW;
        return -1;
    }
    struct tar_header header;
    memset(&header, 0, sizeof(header));
    snprintf(header.name, sizeof(header.name), "%s", name);
    snprintf(header.mode, sizeof(header.mode), "%07o", 0600);
    snprintf(header.uid, sizeof(header.uid), "%07o", 0);
    snprintf(header.gid, sizeof(header.gid), "%07o", 0);
    snprintf(header.size, sizeof(header.size), "%011llo",
             (unsigned long long)length);
    snprintf(header.mtime, sizeof(header.mtime), "%011llo",
             (unsigned long long)timestamp);
    memset(header.checksum, ' ', sizeof(header.checksum));
    header.typeflag = '0';
    memcpy(header.magic, "ustar", 5);
    memcpy(header.version, "00", 2);
    memcpy(header.uname, "hamn", 4);
    memcpy(header.gname, "hamn", 4);

    unsigned checksum = 0;
    const unsigned char *bytes = (const unsigned char *)&header;
    for (size_t i = 0; i < sizeof(header); i++)
        checksum += bytes[i];
    snprintf(header.checksum, 7, "%06o", checksum);
    header.checksum[6] = '\0';
    header.checksum[7] = ' ';

    if (full_write(fd, &header, sizeof(header)) != 0 ||
        full_write(fd, data, length) != 0)
        return -1;
    size_t padding = (512 - (length % 512)) % 512;
    if (padding) {
        static const char zeroes[512];
        if (full_write(fd, zeroes, padding) != 0)
            return -1;
    }
    return 0;
}

static int parent_directory(const char *path, char *directory, size_t capacity)
{
    size_t length = strlen(path);
    if (!length || length >= capacity || path[length - 1] == '/') {
        errno = EINVAL;
        return -1;
    }
    memcpy(directory, path, length + 1);
    char *slash = strrchr(directory, '/');
    if (!slash) {
        snprintf(directory, capacity, ".");
    } else if (slash == directory) {
        slash[1] = '\0';
    } else {
        *slash = '\0';
    }
    return 0;
}

static void fsync_directory(const char *directory)
{
    int fd = open(directory, O_RDONLY | O_CLOEXEC);
    if (fd >= 0) {
        fsync(fd);
        close(fd);
    }
}

/* Writes manifest.json, then every member in order. */
static int create_archive(const char *path, const char *manifest,
                          const struct archive_member members[], size_t count,
                          unsigned long long *size_out)
{
    char directory[PATH_MAX], temporary[PATH_MAX];
    if (parent_directory(path, directory, sizeof(directory)) != 0 ||
        fs_mkdirs(directory, 0700) != 0)
        return -1;

    int fd = -1;
    for (int attempt = 0; attempt < 8; attempt++) {
        uint32_t nonce = arc4random();
        int n = snprintf(temporary, sizeof(temporary), "%s.tmp.%ld.%08x",
                         path, (long)getpid(), nonce);
        if (n < 0 || n >= (int)sizeof(temporary)) {
            errno = ENAMETOOLONG;
            return -1;
        }
        fd = open(temporary,
                  O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW,
                  0600);
        if (fd >= 0)
            break;
        if (errno != EEXIST)
            return -1;
    }
    if (fd < 0)
        return -1;

    int rc = -1;
    time_t timestamp = time(NULL);
    static const char archive_end[1024];
    if (fchmod(fd, 0600) != 0 ||
        tar_add_file(fd, "manifest.json", manifest, strlen(manifest),
                     timestamp) != 0)
        goto out;
    for (size_t index = 0; index < count; index++) {
        if (tar_add_file(fd, members[index].name, members[index].data,
                         members[index].length, timestamp) != 0)
            goto out;
    }
    if (full_write(fd, archive_end, sizeof(archive_end)) != 0 ||
        fsync(fd) != 0)
        goto out;

    off_t size = lseek(fd, 0, SEEK_END);
    if (size < 0 || close(fd) != 0) {
        fd = -1;
        goto out;
    }
    fd = -1;

    /* link(2) publishes atomically without replacing an existing bundle. */
    if (link(temporary, path) != 0)
        goto out;
    if (unlink(temporary) != 0) {
        unlink(path);
        goto out;
    }
    temporary[0] = '\0';
    fsync_directory(directory);
    *size_out = (unsigned long long)size;
    rc = 0;

out:
    if (fd >= 0)
        close(fd);
    if (temporary[0])
        unlink(temporary);
    return rc;
}

static int default_output_path(char *path, size_t capacity)
{
    char home[PATH_MAX], directory[PATH_MAX], timestamp[32];
    if (!hamn_home(home, sizeof(home)))
        return -1;
    int n = snprintf(directory, sizeof(directory), "%s/diagnostics", home);
    if (n < 0 || n >= (int)sizeof(directory) ||
        fs_mkdirs(directory, 0700) != 0)
        return -1;

    time_t now = time(NULL);
    struct tm utc;
    if (!gmtime_r(&now, &utc) ||
        strftime(timestamp, sizeof(timestamp), "%Y%m%dT%H%M%SZ", &utc) == 0)
        return -1;
    n = snprintf(path, capacity, "%s/hamn-diagnostics-%s-%ld.tar",
                 directory, timestamp, (long)getpid());
    if (n < 0 || n >= (int)capacity) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

int hamn_control_diagnostics(const char *profile_name, const char *requested_path,
                              char **output)
{
    if (!output || !profile_name) { errno = EINVAL; return 1; }
    *output = NULL;
    struct profile profile;
    if (profile_read_existing(&profile, profile_name) != 0) {
        logerr("cannot load profile");
        return 1;
    }

    char path[PATH_MAX];
    if (requested_path) {
        int n = snprintf(path, sizeof(path), "%s", requested_path);
        if (!requested_path[0] || n < 0 || n >= (int)sizeof(path)) {
            logerr("invalid diagnostic output path");
            return 1;
        }
    } else if (default_output_path(path, sizeof(path)) != 0) {
        logerr("cannot select diagnostic output path: %s", strerror(errno));
        return 1;
    }

    struct text_buffer serial = { 0 }, serial_previous = { 0 }, vmrun = { 0 };
    struct text_buffer observer = { 0 };
    char *manifest = NULL, *status = NULL, *operation = NULL;
    cJSON *snapshot = NULL;
    int rc = 1;
    if (read_log_tail(&profile, "serial.log", &serial) != 0 ||
        read_log_tail(&profile, "serial.previous.log", &serial_previous) != 0 ||
        read_log_tail(&profile, "vmrun.log", &vmrun) != 0 ||
        read_log_tail(&profile, "port-observer.log", &observer) != 0 ||
        !(snapshot = query_snapshot(&profile)) ||
        !(status = build_status_json(&profile, snapshot)) ||
        !(operation = build_operation_json(snapshot))) {
        logerr("cannot build redacted diagnostic data");
        goto out;
    }
    const struct archive_member members[] = {
        { "status.json", status, strlen(status) },
        { "operation.json", operation, strlen(operation) },
        { "logs/serial.log", serial.data, serial.length },
        { "logs/serial.previous.log", serial_previous.data,
          serial_previous.length },
        { "logs/vmrun.log", vmrun.data, vmrun.length },
        { "logs/port-observer.log", observer.data, observer.length },
    };
    size_t member_count = sizeof(members) / sizeof(members[0]);
    if (!(manifest = build_manifest_json(members, member_count))) {
        logerr("cannot build redacted diagnostic data");
        goto out;
    }

    unsigned long long archive_size = 0;
    if (create_archive(path, manifest, members, member_count,
                       &archive_size) != 0) {
        logerr("cannot create diagnostic archive %s: %s", path,
               strerror(errno));
        goto out;
    }

    cJSON *result = cJSON_CreateObject();
    if (!result || !cJSON_AddNumberToObject(result, "schemaVersion", 1) ||
        !cJSON_AddStringToObject(result, "operation", "diagnostics.create") ||
        !cJSON_AddStringToObject(result, "path", path) ||
        !cJSON_AddStringToObject(result, "format", "ustar") ||
        !cJSON_AddBoolToObject(result, "redacted", 1) ||
        !cJSON_AddNumberToObject(result, "bytes", (double)archive_size)) {
        cJSON_Delete(result);
        unlink(path);
        logerr("cannot encode diagnostic result");
        goto out;
    }
    char *result_text = cJSON_PrintUnformatted(result);
    cJSON_Delete(result);
    if (!result_text) {
        unlink(path);
        logerr("cannot encode diagnostic result");
        goto out;
    }
    *output = result_text;
    rc = 0;

out:
    cJSON_free(manifest);
    cJSON_free(status);
    cJSON_free(operation);
    cJSON_Delete(snapshot);
    buffer_free(&serial);
    buffer_free(&serial_previous);
    buffer_free(&vmrun);
    buffer_free(&observer);
    return rc;
}
