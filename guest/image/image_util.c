#include "image/image_util.h"

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int image_fail(struct image_error *error, const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(error->message, sizeof(error->message), format, arguments);
    va_end(arguments);
    return -1;
}

int image_fail_errno(struct image_error *error, const char *what,
                     const char *path)
{
    return image_fail(error, "%s: %s: %s", what, path, strerror(errno));
}

int image_read_file(const char *path, size_t limit, char **text,
                    size_t *length, struct image_error *error)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return image_fail_errno(error, "cannot open", path);
    char *buffer = malloc(limit + 1);
    if (!buffer) {
        close(fd);
        return image_fail(error, "out of memory reading %s", path);
    }
    size_t used = 0;
    for (;;) {
        ssize_t count = read(fd, buffer + used, limit + 1 - used);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            int saved = errno;
            free(buffer);
            close(fd);
            errno = saved;
            return image_fail_errno(error, "cannot read", path);
        }
        if (count == 0)
            break;
        used += (size_t)count;
        if (used > limit) {
            free(buffer);
            close(fd);
            return image_fail(error, "file is larger than %zu bytes: %s",
                              limit, path);
        }
    }
    close(fd);
    buffer[used] = '\0';
    *text = buffer;
    *length = used;
    return 0;
}

struct json_value *image_read_json(const char *path, struct image_error *error)
{
    char *text = NULL;
    size_t length = 0;
    if (image_read_file(path, IMAGE_JSON_MAX_BYTES, &text, &length, error) != 0)
        return NULL;
    struct json_error json_error;
    struct json_value *value = json_parse(text, length, &json_error);
    free(text);
    if (!value)
        image_fail(error, "%s", json_error.message);
    return value;
}

static int write_all(int fd, const char *bytes, size_t length)
{
    while (length > 0) {
        ssize_t written = write(fd, bytes, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        bytes += written;
        length -= (size_t)written;
    }
    return 0;
}

int image_write_file(const char *path, const char *bytes, size_t length,
                     struct image_error *error)
{
    size_t path_length = strlen(path);
    char *temporary = malloc(path_length + sizeof(".tmp.XXXXXX"));
    if (!temporary)
        return image_fail(error, "out of memory writing %s", path);
    memcpy(temporary, path, path_length);
    memcpy(temporary + path_length, ".tmp.XXXXXX", sizeof(".tmp.XXXXXX"));
    int fd = mkstemp(temporary);
    if (fd < 0) {
        image_fail_errno(error, "cannot create", temporary);
        free(temporary);
        return -1;
    }
    mode_t mask = umask(0);
    umask(mask);
    int failed = fchmod(fd, 0666 & ~mask) != 0 ||
                 write_all(fd, bytes, length) != 0;
    int saved = errno;
    if (close(fd) != 0 && !failed) {
        failed = 1;
        saved = errno;
    }
    if (!failed && rename(temporary, path) != 0) {
        failed = 1;
        saved = errno;
    }
    if (failed) {
        unlink(temporary);
        free(temporary);
        errno = saved;
        return image_fail_errno(error, "cannot write", path);
    }
    free(temporary);
    return 0;
}

int image_write_json(const char *path, const struct json_value *value,
                     int sort_keys, struct image_error *error)
{
    struct json_buffer output = { 0 };
    if (json_dump(value, sort_keys, &output) != 0 ||
        json_buffer_append(&output, "\n", 1) != 0) {
        json_buffer_free(&output);
        return image_fail(error, "cannot serialize %s", path);
    }
    int rc = image_write_file(path, output.data, output.length, error);
    json_buffer_free(&output);
    return rc;
}

int image_lower_hex(const char *text, size_t length, size_t digits)
{
    if (length != digits)
        return 0;
    for (size_t index = 0; index < length; index++) {
        char c = text[index];
        if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')))
            return 0;
    }
    return 1;
}

int image_json_lower_hex(const struct json_value *value, size_t digits)
{
    return value && value->type == JSON_STRING &&
           image_lower_hex(value->text, value->length, digits);
}

int image_owned_single_link(const char *path, struct stat *info)
{
    if (lstat(path, info) != 0)
        return 0;
    return S_ISREG(info->st_mode) && info->st_uid == geteuid() &&
           info->st_nlink == 1 && (info->st_mode & 0022) == 0;
}

long long image_required_savings(long long baseline)
{
    long long percent = baseline / 20 + (baseline % 20 != 0);
    return percent > IMAGE_MINIMUM_SAVINGS ? percent : IMAGE_MINIMUM_SAVINGS;
}
