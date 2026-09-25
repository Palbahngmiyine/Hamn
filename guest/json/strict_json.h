#ifndef HAMN_GUEST_STRICT_JSON_H
#define HAMN_GUEST_STRICT_JSON_H

/*
 * Strict RFC 8259 JSON for Hamn's guest-side tools: the in-guest guest-json
 * helper and the Linux image-builder tool. It replaces Python's json module
 * in those tools, so it keeps the Python semantics they relied on while
 * failing closed where Python emitted invalid JSON.
 *
 * Parsing (json_parse):
 * - Input is LENGTH bytes of UTF-8 (a BOM, overlong forms, encoded surrogates
 *   and code points above U+10FFFF are rejected). Exactly one value, with only
 *   space, tab, LF and CR allowed around tokens.
 * - Duplicate object keys are rejected after unescaping, like Python's
 *   object_pairs_hook checks. NaN/Infinity literals are not JSON and fail.
 * - Numbers keep their exact token text. A token without fraction or exponent
 *   is JSON_INTEGER (any magnitude up to JSON_MAX_INTEGER_DIGITS digits,
 *   Python 3.12's integer string limit); other tokens are JSON_FLOAT and must
 *   convert to a finite double (Python would turn 1e400 into Infinity and
 *   then write invalid JSON).
 * - Strings are decoded into generalized UTF-8: a \u escape naming a lone
 *   UTF-16 surrogate is kept as its three-byte form, as Python keeps it, and
 *   an escaped U+0000 is kept. String bytes are length-delimited; the NUL
 *   terminator after them exists only for convenience.
 * - Nesting deeper than JSON_MAX_DEPTH arrays/objects fails.
 *
 * Writing (json_dump): the exact bytes of Python's
 * json.dumps(value, indent=2, sort_keys=SORT_KEYS) with its default
 * ensure_ascii=True, without a trailing newline: integers keep their digits
 * ("-0" becomes "0"), floats use Python's shortest round-trip repr, strings
 * escape everything outside printable ASCII, and sorted keys use code point
 * order.
 *
 * Ownership: every json_value is owned by exactly one parent, or by the
 * caller for a root. json_free releases a value and all of its children.
 * Functions that take a value (json_array_append, json_object_set) take
 * ownership only on success. Nothing here is thread-safe or uses globals.
 */

#include <stddef.h>

#define JSON_MAX_DEPTH 512
#define JSON_MAX_INTEGER_DIGITS 4300
#define JSON_ERROR_CAP 256

enum json_type {
    JSON_NULL,
    JSON_FALSE,
    JSON_TRUE,
    JSON_INTEGER,
    JSON_FLOAT,
    JSON_STRING,
    JSON_ARRAY,
    JSON_OBJECT,
};

enum json_error_kind {
    JSON_ERROR_NONE,
    JSON_ERROR_SYNTAX,
    JSON_ERROR_DUPLICATE_KEY,
    JSON_ERROR_LIMIT,
    JSON_ERROR_MEMORY,
};

struct json_error {
    enum json_error_kind kind;
    char message[JSON_ERROR_CAP];
};

struct json_value;

struct json_member {
    char *key;            /* NULL for array items */
    size_t key_length;
    struct json_value *value;
};

struct json_value {
    enum json_type type;
    char *text;           /* number token or decoded string, NUL-terminated */
    size_t length;        /* bytes in text before the terminator */
    struct json_member *members; /* array items or object members, in order */
    size_t count;
    size_t capacity;
};

struct json_buffer {
    char *data;           /* NUL-terminated while non-NULL */
    size_t length;
    size_t capacity;
};

/* Returns a new tree, or NULL with ERROR describing the first failure. */
struct json_value *json_parse(const char *text, size_t length,
                              struct json_error *error);
void json_free(struct json_value *value);

/* Constructors return NULL only when memory is exhausted. */
struct json_value *json_new_literal(enum json_type type);
struct json_value *json_new_string(const char *bytes, size_t length);
struct json_value *json_new_cstring(const char *text);
struct json_value *json_new_integer(long long number);
struct json_value *json_new_container(enum json_type type);

/* Both return 0, or -1 without taking ownership (errno ENOMEM or EINVAL). */
int json_array_append(struct json_value *array, struct json_value *item);
/* Replaces an existing member in place (freeing the old value) or appends. */
int json_object_set(struct json_value *object, const char *key,
                    struct json_value *value);

/* KEY is a NUL-terminated key without embedded NUL; NULL when absent. */
struct json_value *json_object_get(const struct json_value *object,
                                   const char *key);
/* Removes and returns a member's value, transferring ownership. */
struct json_value *json_object_take(struct json_value *object, const char *key);

/* 1 when VALUE is a string of exactly the NUL-terminated TEXT. */
int json_string_equals(const struct json_value *value, const char *text);
/*
 * Stores an integer token that fits a long long. Returns -1 for other types,
 * floats and out-of-range integers.
 */
int json_integer_value(const struct json_value *value, long long *out);

/* Appends Python-compatible output; returns -1 (errno) and leaves OUT valid. */
int json_dump(const struct json_value *value, int sort_keys,
              struct json_buffer *out);
/* Appends TEXT as Python's ensure_ascii string body without the quotes. */
int json_escape_ascii(const char *bytes, size_t length, struct json_buffer *out);
int json_buffer_append(struct json_buffer *out, const char *bytes,
                       size_t length);
void json_buffer_free(struct json_buffer *buffer);

/* 1 when BYTES are well-formed UTF-8 without surrogates. */
int json_utf8_valid(const char *bytes, size_t length);

#endif
