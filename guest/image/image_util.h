#ifndef HAMN_IMAGE_UTIL_H
#define HAMN_IMAGE_UTIL_H

/*
 * Shared helpers for hamn-image-tool, the guest image builder and release
 * publication checks. Every fallible function returns 0 on success or -1
 * after writing one human-readable reason into ERROR; callers add their own
 * command prefix. Nothing here keeps global state.
 */

#include <stddef.h>
#include <sys/stat.h>

#include "json/strict_json.h"

#define IMAGE_ERROR_CAP 1024
/* JSON evidence inputs (reports, budgets) are small; images are streamed. */
#define IMAGE_JSON_MAX_BYTES (16 * 1024 * 1024)
#define IMAGE_VIRTUAL_BYTES (8LL * 1024 * 1024 * 1024)
#define IMAGE_RELEASE_ASSET_LIMIT (2LL * 1024 * 1024 * 1024)
#define IMAGE_MINIMUM_SAVINGS (64LL * 1024 * 1024)

struct image_error {
    char message[IMAGE_ERROR_CAP];
};

int image_fail(struct image_error *error, const char *format, ...)
    __attribute__((format(printf, 2, 3)));
/* "WHAT: PATH: strerror(errno)" */
int image_fail_errno(struct image_error *error, const char *what,
                     const char *path);

/* Reads a whole file of at most LIMIT bytes; *TEXT is NUL-terminated. */
int image_read_file(const char *path, size_t limit, char **text,
                    size_t *length, struct image_error *error);
/* Strict JSON (duplicate keys rejected) from PATH; caller frees the tree. */
struct json_value *image_read_json(const char *path, struct image_error *error);
/*
 * Writes BYTES to PATH through a same-directory temporary file and rename,
 * mode 0666 & ~umask like Python's write_text. Replaces an existing file.
 */
int image_write_file(const char *path, const char *bytes, size_t length,
                     struct image_error *error);
/* Writes json_dump(VALUE, SORT_KEYS) plus a newline with image_write_file. */
int image_write_json(const char *path, const struct json_value *value,
                     int sort_keys, struct image_error *error);

/* 1 when TEXT is exactly DIGITS lowercase hexadecimal characters. */
int image_lower_hex(const char *text, size_t length, size_t digits);
/* 1 for a JSON string of exactly DIGITS lowercase hexadecimal characters. */
int image_json_lower_hex(const struct json_value *value, size_t digits);

/*
 * Python's "caller-owned regular single-link source": lstat of PATH is a
 * regular file owned by the effective user, with one link and no group/other
 * write permission. INFO receives that lstat result.
 */
int image_owned_single_link(const char *path, struct stat *info);

/* max(64 MiB, ceil(5% of BASELINE)), the minimum accepted cleanup savings. */
long long image_required_savings(long long baseline);

#endif
