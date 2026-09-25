#include "strict_json.h"

#include <errno.h>
#include <math.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Python's repr switches to exponent form outside this decimal-point range. */
#define PYTHON_REPR_MIN_DECPT (-3)
#define PYTHON_REPR_MAX_DECPT 16
#define DOUBLE_ROUND_TRIP_DIGITS 17

struct parser {
    const unsigned char *text;
    size_t length;
    size_t offset;
    unsigned depth;
    struct json_error *error;
};

int json_buffer_append(struct json_buffer *out, const char *bytes,
                       size_t length)
{
    if (length > SIZE_MAX - out->length - 1) {
        errno = ENOMEM;
        return -1;
    }
    size_t needed = out->length + length + 1;
    if (needed > out->capacity) {
        size_t capacity = out->capacity ? out->capacity : 64;
        while (capacity < needed) {
            if (capacity > SIZE_MAX / 2) {
                capacity = needed;
                break;
            }
            capacity *= 2;
        }
        char *data = realloc(out->data, capacity);
        if (!data) {
            errno = ENOMEM;
            return -1;
        }
        out->data = data;
        out->capacity = capacity;
    }
    if (length)
        memcpy(out->data + out->length, bytes, length);
    out->length += length;
    out->data[out->length] = '\0';
    return 0;
}

static int buffer_append_text(struct json_buffer *out, const char *text)
{
    return json_buffer_append(out, text, strlen(text));
}

void json_buffer_free(struct json_buffer *buffer)
{
    free(buffer->data);
    buffer->data = NULL;
    buffer->length = buffer->capacity = 0;
}

/*
 * Decodes one generalized UTF-8 sequence. ALLOW_SURROGATES admits the
 * three-byte forms of U+D800..U+DFFF that this module stores for lone \u
 * surrogates; raw input never contains them. Returns the length, or 0.
 */
static size_t utf8_decode(const unsigned char *s, size_t n, int allow_surrogates,
                          unsigned *code_point)
{
    if (n == 0)
        return 0;
    unsigned char first = s[0];
    if (first < 0x80) {
        *code_point = first;
        return 1;
    }
    size_t length;
    unsigned minimum, value;
    if (first >= 0xC2 && first <= 0xDF) {
        length = 2;
        minimum = 0x80;
        value = first & 0x1F;
    } else if (first >= 0xE0 && first <= 0xEF) {
        length = 3;
        minimum = 0x800;
        value = first & 0x0F;
    } else if (first >= 0xF0 && first <= 0xF4) {
        length = 4;
        minimum = 0x10000;
        value = first & 0x07;
    } else {
        return 0;
    }
    if (n < length)
        return 0;
    for (size_t index = 1; index < length; index++) {
        if ((s[index] & 0xC0) != 0x80)
            return 0;
        value = (value << 6) | (s[index] & 0x3F);
    }
    if (value < minimum || value > 0x10FFFF)
        return 0;
    if (!allow_surrogates && value >= 0xD800 && value <= 0xDFFF)
        return 0;
    *code_point = value;
    return length;
}

int json_utf8_valid(const char *bytes, size_t length)
{
    const unsigned char *s = (const unsigned char *)bytes;
    size_t offset = 0;
    while (offset < length) {
        unsigned code_point;
        size_t used = utf8_decode(s + offset, length - offset, 0, &code_point);
        if (used == 0)
            return 0;
        offset += used;
    }
    return 1;
}

static int utf8_append(struct json_buffer *out, unsigned code_point)
{
    char bytes[4];
    size_t length;
    if (code_point < 0x80) {
        bytes[0] = (char)code_point;
        length = 1;
    } else if (code_point < 0x800) {
        bytes[0] = (char)(0xC0 | (code_point >> 6));
        bytes[1] = (char)(0x80 | (code_point & 0x3F));
        length = 2;
    } else if (code_point < 0x10000) {
        bytes[0] = (char)(0xE0 | (code_point >> 12));
        bytes[1] = (char)(0x80 | ((code_point >> 6) & 0x3F));
        bytes[2] = (char)(0x80 | (code_point & 0x3F));
        length = 3;
    } else {
        bytes[0] = (char)(0xF0 | (code_point >> 18));
        bytes[1] = (char)(0x80 | ((code_point >> 12) & 0x3F));
        bytes[2] = (char)(0x80 | ((code_point >> 6) & 0x3F));
        bytes[3] = (char)(0x80 | (code_point & 0x3F));
        length = 4;
    }
    return json_buffer_append(out, bytes, length);
}

static void set_error(struct json_error *error, enum json_error_kind kind,
                      const char *format, ...)
    __attribute__((format(printf, 3, 4)));

static void set_error(struct json_error *error, enum json_error_kind kind,
                      const char *format, ...)
{
    if (!error)
        return;
    error->kind = kind;
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(error->message, sizeof(error->message), format, arguments);
    va_end(arguments);
}

static void *syntax_error(struct parser *p, const char *reason)
{
    set_error(p->error, JSON_ERROR_SYNTAX, "invalid JSON at byte %zu: %s",
              p->offset, reason);
    return NULL;
}

static void *memory_error(struct parser *p)
{
    set_error(p->error, JSON_ERROR_MEMORY, "out of memory");
    return NULL;
}

static void skip_whitespace(struct parser *p)
{
    while (p->offset < p->length) {
        unsigned char c = p->text[p->offset];
        if (c != ' ' && c != '\t' && c != '\n' && c != '\r')
            return;
        p->offset++;
    }
}

static int peek(const struct parser *p)
{
    return p->offset < p->length ? p->text[p->offset] : -1;
}

static struct json_value *value_new(enum json_type type)
{
    struct json_value *value = calloc(1, sizeof(*value));
    if (value)
        value->type = type;
    return value;
}

void json_free(struct json_value *value)
{
    if (!value)
        return;
    for (size_t index = 0; index < value->count; index++) {
        free(value->members[index].key);
        json_free(value->members[index].value);
    }
    free(value->members);
    free(value->text);
    free(value);
}

static int members_reserve(struct json_value *container)
{
    if (container->count < container->capacity)
        return 0;
    size_t capacity = container->capacity ? container->capacity * 2 : 8;
    if (capacity > SIZE_MAX / sizeof(*container->members)) {
        errno = ENOMEM;
        return -1;
    }
    struct json_member *members =
        realloc(container->members, capacity * sizeof(*members));
    if (!members) {
        errno = ENOMEM;
        return -1;
    }
    container->members = members;
    container->capacity = capacity;
    return 0;
}

static int hex_digit(int c)
{
    if (c >= '0' && c <= '9')
        return c - '0';
    if (c >= 'a' && c <= 'f')
        return c - 'a' + 10;
    if (c >= 'A' && c <= 'F')
        return c - 'A' + 10;
    return -1;
}

/* Reads four hex digits at OFFSET; -1 when they are missing or invalid. */
static long hex4_at(const struct parser *p, size_t offset)
{
    if (offset > p->length || p->length - offset < 4)
        return -1;
    long value = 0;
    for (size_t index = 0; index < 4; index++) {
        int digit = hex_digit(p->text[offset + index]);
        if (digit < 0)
            return -1;
        value = value * 16 + digit;
    }
    return value;
}

/* Parses a string at '"' into OUT (generalized UTF-8, NUL-terminated). */
static int parse_string_bytes(struct parser *p, struct json_buffer *out)
{
    p->offset++;
    for (;;) {
        if (p->offset >= p->length) {
            syntax_error(p, "unterminated string");
            return -1;
        }
        unsigned char c = p->text[p->offset];
        if (c == '"') {
            p->offset++;
            if (!out->data && json_buffer_append(out, "", 0) != 0) {
                memory_error(p);
                return -1;
            }
            return 0;
        }
        if (c < 0x20) {
            syntax_error(p, "control character in string");
            return -1;
        }
        if (c != '\\') {
            size_t start = p->offset;
            while (p->offset < p->length && p->text[p->offset] >= 0x20 &&
                   p->text[p->offset] != '"' && p->text[p->offset] != '\\')
                p->offset++;
            if (json_buffer_append(out, (const char *)p->text + start,
                                   p->offset - start) != 0) {
                memory_error(p);
                return -1;
            }
            continue;
        }
        if (p->offset + 1 >= p->length) {
            p->offset++;
            syntax_error(p, "unterminated string");
            return -1;
        }
        unsigned char escape = p->text[p->offset + 1];
        const char *simple = NULL;
        switch (escape) {
        case '"': simple = "\""; break;
        case '\\': simple = "\\"; break;
        case '/': simple = "/"; break;
        case 'b': simple = "\b"; break;
        case 'f': simple = "\f"; break;
        case 'n': simple = "\n"; break;
        case 'r': simple = "\r"; break;
        case 't': simple = "\t"; break;
        case 'u': break;
        default:
            syntax_error(p, "invalid escape");
            return -1;
        }
        if (simple) {
            p->offset += 2;
            if (json_buffer_append(out, simple, 1) != 0) {
                memory_error(p);
                return -1;
            }
            continue;
        }
        long code_point = hex4_at(p, p->offset + 2);
        if (code_point < 0) {
            syntax_error(p, "invalid \\u escape");
            return -1;
        }
        p->offset += 6;
        /* Python combines only an immediately following low surrogate. */
        if (code_point >= 0xD800 && code_point <= 0xDBFF &&
            p->offset + 1 < p->length && p->text[p->offset] == '\\' &&
            p->text[p->offset + 1] == 'u') {
            long low = hex4_at(p, p->offset + 2);
            if (low < 0) {
                syntax_error(p, "invalid \\u escape");
                return -1;
            }
            if (low >= 0xDC00 && low <= 0xDFFF) {
                code_point = 0x10000 + ((code_point - 0xD800) << 10) +
                             (low - 0xDC00);
                p->offset += 6;
            }
        }
        if (utf8_append(out, (unsigned)code_point) != 0) {
            memory_error(p);
            return -1;
        }
    }
}

static struct json_value *parse_value(struct parser *p);

static int member_order(const void *left, const void *right)
{
    const struct json_member *a = *(const struct json_member *const *)left;
    const struct json_member *b = *(const struct json_member *const *)right;
    size_t shorter = a->key_length < b->key_length ? a->key_length : b->key_length;
    int order = memcmp(a->key, b->key, shorter);
    if (order != 0)
        return order;
    return (a->key_length > b->key_length) - (a->key_length < b->key_length);
}

/* Returns member pointers in code point order; caller frees the array. */
static struct json_member **sorted_members(const struct json_value *object)
{
    struct json_member **order = malloc((object->count ? object->count : 1) *
                                        sizeof(*order));
    if (!order)
        return NULL;
    for (size_t index = 0; index < object->count; index++)
        order[index] = &object->members[index];
    qsort(order, object->count, sizeof(*order), member_order);
    return order;
}

static int reject_duplicate_keys(struct parser *p, const struct json_value *object)
{
    if (object->count < 2)
        return 0;
    struct json_member **order = sorted_members(object);
    if (!order) {
        memory_error(p);
        return -1;
    }
    for (size_t index = 1; index < object->count; index++) {
        if (member_order(&order[index - 1], &order[index]) != 0)
            continue;
        struct json_buffer key = { 0 };
        if (json_escape_ascii(order[index]->key, order[index]->key_length,
                              &key) != 0) {
            free(order);
            json_buffer_free(&key);
            memory_error(p);
            return -1;
        }
        set_error(p->error, JSON_ERROR_DUPLICATE_KEY, "duplicate key: %s",
                  key.data ? key.data : "");
        json_buffer_free(&key);
        free(order);
        return -1;
    }
    free(order);
    return 0;
}

static struct json_value *parse_container(struct parser *p, enum json_type type)
{
    if (p->depth >= JSON_MAX_DEPTH) {
        set_error(p->error, JSON_ERROR_LIMIT,
                  "invalid JSON at byte %zu: nesting is deeper than %d",
                  p->offset, JSON_MAX_DEPTH);
        return NULL;
    }
    int close = type == JSON_OBJECT ? '}' : ']';
    struct json_value *container = value_new(type);
    if (!container)
        return memory_error(p);
    p->depth++;
    p->offset++;
    skip_whitespace(p);
    if (peek(p) == close) {
        p->offset++;
        p->depth--;
        return container;
    }
    for (;;) {
        struct json_buffer key = { 0 };
        if (type == JSON_OBJECT) {
            if (peek(p) != '"') {
                syntax_error(p, "expected a string key");
                goto fail;
            }
            if (parse_string_bytes(p, &key) != 0)
                goto fail;
            skip_whitespace(p);
            if (peek(p) != ':') {
                json_buffer_free(&key);
                syntax_error(p, "expected ':'");
                goto fail;
            }
            p->offset++;
            skip_whitespace(p);
        }
        struct json_value *item = parse_value(p);
        if (!item) {
            json_buffer_free(&key);
            goto fail;
        }
        if (members_reserve(container) != 0) {
            json_buffer_free(&key);
            json_free(item);
            memory_error(p);
            goto fail;
        }
        struct json_member *member = &container->members[container->count++];
        member->key = key.data;
        member->key_length = key.length;
        member->value = item;
        skip_whitespace(p);
        int next = peek(p);
        if (next == ',') {
            p->offset++;
            skip_whitespace(p);
            continue;
        }
        if (next == close) {
            p->offset++;
            break;
        }
        syntax_error(p, type == JSON_OBJECT ? "expected ',' or '}'" :
                                              "expected ',' or ']'");
        goto fail;
    }
    if (type == JSON_OBJECT && reject_duplicate_keys(p, container) != 0)
        goto fail;
    p->depth--;
    return container;

fail:
    json_free(container);
    return NULL;
}

static int is_digit(int c)
{
    return c >= '0' && c <= '9';
}

static struct json_value *parse_number(struct parser *p)
{
    size_t start = p->offset;
    int integer = 1;
    if (peek(p) == '-')
        p->offset++;
    if (peek(p) == '0') {
        p->offset++;
    } else if (peek(p) >= '1' && peek(p) <= '9') {
        while (is_digit(peek(p)))
            p->offset++;
    } else {
        return syntax_error(p, "invalid number");
    }
    if (peek(p) == '.') {
        integer = 0;
        p->offset++;
        if (!is_digit(peek(p)))
            return syntax_error(p, "invalid number fraction");
        while (is_digit(peek(p)))
            p->offset++;
    }
    if (peek(p) == 'e' || peek(p) == 'E') {
        integer = 0;
        p->offset++;
        if (peek(p) == '+' || peek(p) == '-')
            p->offset++;
        if (!is_digit(peek(p)))
            return syntax_error(p, "invalid number exponent");
        while (is_digit(peek(p)))
            p->offset++;
    }
    size_t length = p->offset - start;
    struct json_value *value = value_new(integer ? JSON_INTEGER : JSON_FLOAT);
    char *text = malloc(length + 1);
    if (!value || !text) {
        free(value);
        free(text);
        return memory_error(p);
    }
    memcpy(text, p->text + start, length);
    text[length] = '\0';
    value->text = text;
    value->length = length;
    if (integer) {
        size_t digits = length - (text[0] == '-');
        if (digits > JSON_MAX_INTEGER_DIGITS) {
            json_free(value);
            set_error(p->error, JSON_ERROR_LIMIT,
                      "invalid JSON at byte %zu: integer has more than %d digits",
                      start, JSON_MAX_INTEGER_DIGITS);
            return NULL;
        }
    } else {
        char *end = NULL;
        double number = strtod(text, &end);
        if (end != text + length || !isfinite(number)) {
            json_free(value);
            p->offset = start;
            return syntax_error(p, "number is outside the finite double range");
        }
    }
    return value;
}

static struct json_value *parse_literal(struct parser *p, const char *word,
                                        enum json_type type)
{
    size_t length = strlen(word);
    if (p->length - p->offset < length ||
        memcmp(p->text + p->offset, word, length) != 0)
        return syntax_error(p, "expected a JSON value");
    p->offset += length;
    struct json_value *value = value_new(type);
    return value ? value : memory_error(p);
}

static struct json_value *parse_value(struct parser *p)
{
    int c = peek(p);
    switch (c) {
    case '{':
        return parse_container(p, JSON_OBJECT);
    case '[':
        return parse_container(p, JSON_ARRAY);
    case '"': {
        struct json_buffer bytes = { 0 };
        if (parse_string_bytes(p, &bytes) != 0) {
            json_buffer_free(&bytes);
            return NULL;
        }
        struct json_value *value = value_new(JSON_STRING);
        if (!value) {
            json_buffer_free(&bytes);
            return memory_error(p);
        }
        value->text = bytes.data;
        value->length = bytes.length;
        return value;
    }
    case 't':
        return parse_literal(p, "true", JSON_TRUE);
    case 'f':
        return parse_literal(p, "false", JSON_FALSE);
    case 'n':
        return parse_literal(p, "null", JSON_NULL);
    default:
        if (c == '-' || is_digit(c))
            return parse_number(p);
        if (c < 0)
            return syntax_error(p, "unexpected end of input");
        return syntax_error(p, "expected a JSON value");
    }
}

struct json_value *json_parse(const char *text, size_t length,
                              struct json_error *error)
{
    struct json_error ignored;
    if (!error)
        error = &ignored;
    error->kind = JSON_ERROR_NONE;
    error->message[0] = '\0';
    struct parser p = {
        .text = (const unsigned char *)text,
        .length = length,
        .error = error,
    };
    if (!text || !json_utf8_valid(text, length)) {
        set_error(error, JSON_ERROR_SYNTAX, "input is not valid UTF-8");
        return NULL;
    }
    skip_whitespace(&p);
    struct json_value *value = parse_value(&p);
    if (!value)
        return NULL;
    skip_whitespace(&p);
    if (p.offset != p.length) {
        json_free(value);
        return syntax_error(&p, "extra data after the JSON value");
    }
    return value;
}

struct json_value *json_new_literal(enum json_type type)
{
    if (type != JSON_NULL && type != JSON_TRUE && type != JSON_FALSE) {
        errno = EINVAL;
        return NULL;
    }
    return value_new(type);
}

struct json_value *json_new_string(const char *bytes, size_t length)
{
    struct json_value *value = value_new(JSON_STRING);
    char *text = malloc(length + 1);
    if (!value || !text) {
        free(value);
        free(text);
        errno = ENOMEM;
        return NULL;
    }
    if (length)
        memcpy(text, bytes, length);
    text[length] = '\0';
    value->text = text;
    value->length = length;
    return value;
}

struct json_value *json_new_cstring(const char *text)
{
    return json_new_string(text, strlen(text));
}

struct json_value *json_new_integer(long long number)
{
    char digits[32];
    int length = snprintf(digits, sizeof(digits), "%lld", number);
    struct json_value *value = value_new(JSON_INTEGER);
    char *text = length > 0 ? strdup(digits) : NULL;
    if (!value || !text) {
        free(value);
        free(text);
        errno = ENOMEM;
        return NULL;
    }
    value->text = text;
    value->length = (size_t)length;
    return value;
}

struct json_value *json_new_container(enum json_type type)
{
    if (type != JSON_ARRAY && type != JSON_OBJECT) {
        errno = EINVAL;
        return NULL;
    }
    return value_new(type);
}

int json_array_append(struct json_value *array, struct json_value *item)
{
    if (!array || array->type != JSON_ARRAY || !item) {
        errno = EINVAL;
        return -1;
    }
    if (members_reserve(array) != 0)
        return -1;
    array->members[array->count].key = NULL;
    array->members[array->count].key_length = 0;
    array->members[array->count].value = item;
    array->count++;
    return 0;
}

static struct json_member *find_member(const struct json_value *object,
                                       const char *key)
{
    if (!object || object->type != JSON_OBJECT)
        return NULL;
    size_t length = strlen(key);
    for (size_t index = 0; index < object->count; index++) {
        struct json_member *member = &object->members[index];
        if (member->key_length == length &&
            memcmp(member->key, key, length) == 0)
            return member;
    }
    return NULL;
}

int json_object_set(struct json_value *object, const char *key,
                    struct json_value *value)
{
    if (!object || object->type != JSON_OBJECT || !key || !value) {
        errno = EINVAL;
        return -1;
    }
    struct json_member *existing = find_member(object, key);
    if (existing) {
        json_free(existing->value);
        existing->value = value;
        return 0;
    }
    char *copy = strdup(key);
    if (!copy || members_reserve(object) != 0) {
        free(copy);
        errno = ENOMEM;
        return -1;
    }
    struct json_member *member = &object->members[object->count++];
    member->key = copy;
    member->key_length = strlen(copy);
    member->value = value;
    return 0;
}

struct json_value *json_object_get(const struct json_value *object,
                                   const char *key)
{
    struct json_member *member = find_member(object, key);
    return member ? member->value : NULL;
}

struct json_value *json_object_take(struct json_value *object, const char *key)
{
    struct json_member *member = find_member(object, key);
    if (!member)
        return NULL;
    struct json_value *value = member->value;
    free(member->key);
    size_t index = (size_t)(member - object->members);
    memmove(member, member + 1,
            (object->count - index - 1) * sizeof(*member));
    object->count--;
    return value;
}

int json_string_equals(const struct json_value *value, const char *text)
{
    if (!value || value->type != JSON_STRING)
        return 0;
    size_t length = strlen(text);
    return value->length == length && memcmp(value->text, text, length) == 0;
}

int json_integer_value(const struct json_value *value, long long *out)
{
    if (!value || value->type != JSON_INTEGER)
        return -1;
    errno = 0;
    char *end = NULL;
    long long number = strtoll(value->text, &end, 10);
    if (errno == ERANGE || end != value->text + value->length)
        return -1;
    *out = number;
    return 0;
}

int json_escape_ascii(const char *bytes, size_t length, struct json_buffer *out)
{
    const unsigned char *s = (const unsigned char *)bytes;
    size_t offset = 0;
    if (!out->data && json_buffer_append(out, "", 0) != 0)
        return -1;
    while (offset < length) {
        unsigned code_point;
        size_t used = utf8_decode(s + offset, length - offset, 1, &code_point);
        if (used == 0) {
            /* Only this module's decoded strings reach here. */
            errno = EILSEQ;
            return -1;
        }
        offset += used;
        char escaped[16];
        const char *text = escaped;
        switch (code_point) {
        case '"': text = "\\\""; break;
        case '\\': text = "\\\\"; break;
        case '\n': text = "\\n"; break;
        case '\r': text = "\\r"; break;
        case '\t': text = "\\t"; break;
        case '\b': text = "\\b"; break;
        case '\f': text = "\\f"; break;
        default:
            if (code_point >= 0x20 && code_point <= 0x7E) {
                escaped[0] = (char)code_point;
                escaped[1] = '\0';
            } else if (code_point < 0x10000) {
                snprintf(escaped, sizeof(escaped), "\\u%04x", code_point);
            } else {
                unsigned value = code_point - 0x10000;
                snprintf(escaped, sizeof(escaped), "\\u%04x\\u%04x",
                         0xD800 | (value >> 10), 0xDC00 | (value & 0x3FF));
            }
        }
        if (buffer_append_text(out, text) != 0)
            return -1;
    }
    return 0;
}

static int append_quoted(struct json_buffer *out, const char *bytes,
                         size_t length)
{
    if (json_buffer_append(out, "\"", 1) != 0 ||
        json_escape_ascii(bytes, length, out) != 0 ||
        json_buffer_append(out, "\"", 1) != 0)
        return -1;
    return 0;
}

/*
 * Splits a "%.Ne" rendering into significant DIGITS and the decimal
 * exponent of the first digit.
 */
static void split_exponent(const char *rendered, char *digits, size_t *count,
                           int *exponent)
{
    size_t used = 0;
    const char *cursor = rendered;
    for (; *cursor && *cursor != 'e'; cursor++) {
        if (*cursor >= '0' && *cursor <= '9')
            digits[used++] = *cursor;
    }
    digits[used] = '\0';
    *count = used;
    *exponent = *cursor == 'e' ? atoi(cursor + 1) : 0;
}

static double digits_value(const char *digits, size_t count, int exponent)
{
    char text[64];
    int written = snprintf(text, sizeof(text), "%c.%se%d", digits[0],
                           count > 1 ? digits + 1 : "", exponent);
    if (written < 0 || (size_t)written >= sizeof(text))
        return NAN;
    return strtod(text, NULL);
}

/* Moves DIGITS to the neighbouring decimal with the same digit count. */
static void step_digits(char *digits, size_t count, int *exponent, int upward)
{
    if (upward) {
        size_t index = count;
        while (index > 0) {
            index--;
            if (digits[index] != '9') {
                digits[index]++;
                return;
            }
            digits[index] = '0';
        }
        digits[0] = '1';
        (*exponent)++;
        return;
    }
    size_t index = count;
    while (index > 0) {
        index--;
        if (digits[index] != '0') {
            digits[index]--;
            break;
        }
        digits[index] = '9';
    }
    if (digits[0] == '0') {
        memset(digits, '9', count);
        (*exponent)--;
    }
}

/*
 * Shortest significant digits that round-trip VALUE (> 0), choosing the
 * nearest such decimal as Python's float repr does. The correctly rounded
 * N-digit rendering is the nearest N-digit decimal; its neighbour on the
 * other side of VALUE is the only other N-digit candidate that can round-trip
 * (this matters at powers of two, where the rounding interval is asymmetric).
 */
static int shortest_digits(double value, char digits[32], size_t *count,
                           int *exponent)
{
    for (int precision = 1; precision <= DOUBLE_ROUND_TRIP_DIGITS; precision++) {
        char rendered[64];
        snprintf(rendered, sizeof(rendered), "%.*e", precision - 1, value);
        split_exponent(rendered, digits, count, exponent);
        double nearest = strtod(rendered, NULL);
        if (nearest == value)
            return 0;
        step_digits(digits, *count, exponent, nearest < value);
        if (digits_value(digits, *count, *exponent) == value)
            return 0;
    }
    errno = ERANGE;
    return -1;
}

static int append_python_float(struct json_buffer *out, const char *token)
{
    double value = strtod(token, NULL);
    if (!isfinite(value)) {
        errno = ERANGE;
        return -1;
    }
    if (signbit(value) && json_buffer_append(out, "-", 1) != 0)
        return -1;
    value = fabs(value);
    char digits[32];
    size_t count;
    int exponent;
    if (value == 0) {
        strcpy(digits, "0");
        count = 1;
        exponent = 0;
    } else if (shortest_digits(value, digits, &count, &exponent) != 0) {
        return -1;
    }
    while (count > 1 && digits[count - 1] == '0')
        digits[--count] = '\0';
    int decimal_point = exponent + 1;
    char text[64];
    if (decimal_point < PYTHON_REPR_MIN_DECPT ||
        decimal_point > PYTHON_REPR_MAX_DECPT) {
        snprintf(text, sizeof(text), "%c%s%se%c%02d", digits[0],
                 count > 1 ? "." : "", count > 1 ? digits + 1 : "",
                 exponent < 0 ? '-' : '+', exponent < 0 ? -exponent : exponent);
        return buffer_append_text(out, text);
    }
    if (decimal_point <= 0) {
        if (buffer_append_text(out, "0.") != 0)
            return -1;
        for (int index = decimal_point; index < 0; index++) {
            if (json_buffer_append(out, "0", 1) != 0)
                return -1;
        }
        return json_buffer_append(out, digits, count);
    }
    if ((size_t)decimal_point >= count) {
        if (json_buffer_append(out, digits, count) != 0)
            return -1;
        for (size_t index = count; index < (size_t)decimal_point; index++) {
            if (json_buffer_append(out, "0", 1) != 0)
                return -1;
        }
        return buffer_append_text(out, ".0");
    }
    if (json_buffer_append(out, digits, (size_t)decimal_point) != 0 ||
        json_buffer_append(out, ".", 1) != 0)
        return -1;
    return json_buffer_append(out, digits + decimal_point,
                              count - (size_t)decimal_point);
}

static int append_indent(struct json_buffer *out, unsigned level)
{
    if (json_buffer_append(out, "\n", 1) != 0)
        return -1;
    for (unsigned index = 0; index < level; index++) {
        if (json_buffer_append(out, "  ", 2) != 0)
            return -1;
    }
    return 0;
}

static int dump_value(const struct json_value *value, int sort_keys,
                      unsigned level, struct json_buffer *out)
{
    if (!value || level > JSON_MAX_DEPTH) {
        errno = EINVAL;
        return -1;
    }
    switch (value->type) {
    case JSON_NULL:
        return buffer_append_text(out, "null");
    case JSON_TRUE:
        return buffer_append_text(out, "true");
    case JSON_FALSE:
        return buffer_append_text(out, "false");
    case JSON_INTEGER:
        /* Python's int("-0") is 0; every other token is already canonical. */
        if (strcmp(value->text, "-0") == 0)
            return buffer_append_text(out, "0");
        return json_buffer_append(out, value->text, value->length);
    case JSON_FLOAT:
        return append_python_float(out, value->text);
    case JSON_STRING:
        return append_quoted(out, value->text, value->length);
    case JSON_ARRAY:
    case JSON_OBJECT:
        break;
    default:
        errno = EINVAL;
        return -1;
    }
    int object = value->type == JSON_OBJECT;
    if (value->count == 0)
        return buffer_append_text(out, object ? "{}" : "[]");
    struct json_member **order = NULL;
    if (object && sort_keys && !(order = sorted_members(value))) {
        errno = ENOMEM;
        return -1;
    }
    int rc = json_buffer_append(out, object ? "{" : "[", 1);
    for (size_t index = 0; rc == 0 && index < value->count; index++) {
        const struct json_member *member =
            order ? order[index] : &value->members[index];
        if (index > 0)
            rc = json_buffer_append(out, ",", 1);
        if (rc == 0)
            rc = append_indent(out, level + 1);
        if (rc == 0 && object) {
            rc = append_quoted(out, member->key, member->key_length);
            if (rc == 0)
                rc = json_buffer_append(out, ": ", 2);
        }
        if (rc == 0)
            rc = dump_value(member->value, sort_keys, level + 1, out);
    }
    if (rc == 0)
        rc = append_indent(out, level);
    if (rc == 0)
        rc = json_buffer_append(out, object ? "}" : "]", 1);
    free(order);
    return rc;
}

int json_dump(const struct json_value *value, int sort_keys,
              struct json_buffer *out)
{
    return dump_value(value, sort_keys, 0, out);
}
