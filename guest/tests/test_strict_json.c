/*
 * Contract tests for guest/json/strict_json: RFC 8259 acceptance, duplicate
 * keys, limits, and byte-exact Python json.dumps(indent=2) output. Expected
 * strings come from Python's documented json/float repr behavior, not from
 * running this implementation.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "json/strict_json.h"

static int failures;

#define CHECK(condition)                                                     \
    do {                                                                     \
        if (!(condition)) {                                                  \
            fprintf(stderr, "FAIL: %s:%d: %s\n", __FILE__, __LINE__,          \
                    #condition);                                             \
            failures++;                                                      \
        }                                                                    \
    } while (0)

static struct json_value *parse_text(const char *text, struct json_error *error)
{
    return json_parse(text, strlen(text), error);
}

static void expect_rejected(const char *text, size_t length,
                            enum json_error_kind kind)
{
    struct json_error error;
    struct json_value *value = json_parse(text, length, &error);
    if (value || error.kind != kind) {
        fprintf(stderr, "FAIL: accepted or misclassified input: ");
        fwrite(text, 1, length, stderr);
        fprintf(stderr, " (kind %d: %s)\n", error.kind, error.message);
        failures++;
    }
    json_free(value);
}

static void rejected(const char *text)
{
    expect_rejected(text, strlen(text), JSON_ERROR_SYNTAX);
}

/* Parses TEXT and compares json_dump(SORT) with EXPECTED exactly. */
static void expect_dump(const char *text, int sort, const char *expected)
{
    struct json_error error;
    struct json_value *value = parse_text(text, &error);
    struct json_buffer out = { 0 };
    if (!value || json_dump(value, sort, &out) != 0 ||
        strcmp(out.data, expected) != 0) {
        fprintf(stderr, "FAIL: dump of %s\n  expected: %s\n  actual:   %s\n%s\n",
                text, expected, out.data ? out.data : "(none)",
                value ? "" : error.message);
        failures++;
    }
    json_buffer_free(&out);
    json_free(value);
}

static void test_types_and_tokens(void)
{
    struct json_error error;
    struct json_value *value = parse_text(
        " {\"i\":-12,\"f\":1.50,\"e\":1E2,\"big\":123456789012345678901234567890,"
        "\"t\":true,\"n\":null,\"s\":\"x\"} \t\r\n", &error);
    CHECK(value && value->type == JSON_OBJECT && value->count == 7);
    CHECK(json_object_get(value, "i")->type == JSON_INTEGER);
    CHECK(strcmp(json_object_get(value, "i")->text, "-12") == 0);
    CHECK(json_object_get(value, "f")->type == JSON_FLOAT);
    CHECK(strcmp(json_object_get(value, "f")->text, "1.50") == 0);
    CHECK(json_object_get(value, "e")->type == JSON_FLOAT);
    CHECK(json_object_get(value, "t")->type == JSON_TRUE);
    CHECK(json_object_get(value, "n")->type == JSON_NULL);
    long long number = 0;
    CHECK(json_integer_value(json_object_get(value, "i"), &number) == 0 &&
          number == -12);
    CHECK(json_integer_value(json_object_get(value, "f"), &number) != 0);
    CHECK(json_integer_value(json_object_get(value, "big"), &number) != 0);
    CHECK(json_integer_value(json_object_get(value, "t"), &number) != 0);
    CHECK(json_string_equals(json_object_get(value, "s"), "x"));
    CHECK(!json_string_equals(json_object_get(value, "i"), "-12"));
    json_free(value);

    /* Embedded NUL survives and never equals its C-string prefix. */
    value = parse_text("\"a\\u0000b\"", &error);
    CHECK(value && value->length == 3 && memcmp(value->text, "a\0b", 3) == 0);
    CHECK(!json_string_equals(value, "a"));
    json_free(value);
}

static void test_rejections(void)
{
    static const char *const syntax[] = {
        "", " ", "01", "-01", "1.", ".5", "+1", "1e", "1e+", "-", "--1",
        "NaN", "Infinity", "-Infinity", "[1,]", "{\"a\":1,}", "{,}", "[",
        "]", "{\"a\"}", "{\"a\" 1}", "{1:2}", "\"\\x\"", "\"\\u12\"",
        "\"\\ud800\\u12\"", "\"abc", "tru", "nul", "falsey", "1 2", "[] []",
        "\v1", "\f1", "1\v", "[1 2]", "{\"a\":1 \"b\":2}", "'a'", "\"\\\"",
        "1e400", "-1e400", "[1e999]",
    };
    for (size_t index = 0; index < sizeof(syntax) / sizeof(syntax[0]); index++)
        rejected(syntax[index]);
    /* Raw control characters and invalid UTF-8, including a BOM. */
    expect_rejected("\"a\x01\"", 4, JSON_ERROR_SYNTAX);
    expect_rejected("\"a\nb\"", 5, JSON_ERROR_SYNTAX);
    expect_rejected("\xef\xbb\xbf{}", 5, JSON_ERROR_SYNTAX);
    expect_rejected("\"\xff\"", 3, JSON_ERROR_SYNTAX);
    expect_rejected("\"\xc0\x80\"", 4, JSON_ERROR_SYNTAX);
    expect_rejected("\"\xed\xa0\x80\"", 5, JSON_ERROR_SYNTAX);
    expect_rejected("\"\xf4\x90\x80\x80\"", 6, JSON_ERROR_SYNTAX);
    expect_rejected("\"\xe2\x82\"", 4, JSON_ERROR_SYNTAX);
    expect_rejected("1\0", 2, JSON_ERROR_SYNTAX);
}

static void test_delete_character_is_valid(void)
{
    struct json_error error;
    struct json_value *value = json_parse("\"\x7f\"", 3, &error);
    CHECK(value && value->length == 1);
    json_free(value);
}

static void test_duplicates(void)
{
    static const char *const duplicates[] = {
        "{\"a\":1,\"a\":2}",
        "{\"x\":{\"a\":1,\"b\":2,\"a\":3}}",
        "[{\"k\":1},{\"k\":1,\"k\":1}]",
        "{\"a\":1,\"\\u0061\":2}",
        "{\"\xf0\x9f\x98\x80\":1,\"\\ud83d\\ude00\":2}",
        "{\"\\ud800\":1,\"\\ud800\":2}",
    };
    for (size_t index = 0; index < sizeof(duplicates) / sizeof(duplicates[0]);
         index++)
        expect_rejected(duplicates[index], strlen(duplicates[index]),
                        JSON_ERROR_DUPLICATE_KEY);
    struct json_error error;
    struct json_value *value = parse_text("{\"debug\":true,\"debug\":false}",
                                          &error);
    CHECK(!value && strcmp(error.message, "duplicate key: debug") == 0);
    /* Distinct keys, including a lone surrogate beside a combined pair. */
    value = parse_text("{\"a\":1,\"A\":2,\"\\ud83d\":3,\"\\ud83d\\ude00\":4,"
                       "\"a\\u0000\":5}", &error);
    CHECK(value && value->count == 5);
    json_free(value);
}

static void test_limits(void)
{
    char *deep = malloc(2 * (JSON_MAX_DEPTH + 1) + 1);
    CHECK(deep != NULL);
    if (!deep)
        return;
    for (int depth = JSON_MAX_DEPTH; depth <= JSON_MAX_DEPTH + 1; depth++) {
        memset(deep, '[', (size_t)depth);
        memset(deep + depth, ']', (size_t)depth);
        struct json_error error;
        struct json_value *value = json_parse(deep, 2 * (size_t)depth, &error);
        if (depth == JSON_MAX_DEPTH)
            CHECK(value != NULL);
        else
            CHECK(!value && error.kind == JSON_ERROR_LIMIT);
        json_free(value);
    }
    free(deep);

    char digits[JSON_MAX_INTEGER_DIGITS + 3];
    memset(digits, '7', sizeof(digits));
    digits[0] = '-';
    struct json_error error;
    struct json_value *value = json_parse(digits, JSON_MAX_INTEGER_DIGITS + 1,
                                          &error);
    CHECK(value && value->type == JSON_INTEGER);
    json_free(value);
    value = json_parse(digits, JSON_MAX_INTEGER_DIGITS + 2, &error);
    CHECK(!value && error.kind == JSON_ERROR_LIMIT);
}

static void test_python_layout(void)
{
    expect_dump("{}", 1, "{}");
    expect_dump("[]", 1, "[]");
    expect_dump("{\"a\":[],\"b\":{}}", 1, "{\n  \"a\": [],\n  \"b\": {}\n}");
    expect_dump("{\"b\":1,\"a\":[1,{\"d\":null,\"c\":true}]}", 1,
                "{\n  \"a\": [\n    1,\n    {\n      \"c\": true,\n"
                "      \"d\": null\n    }\n  ],\n  \"b\": 1\n}");
    /* Insertion order is kept when sorting is off. */
    expect_dump("{\"b\":1,\"a\":false}", 0,
                "{\n  \"b\": 1,\n  \"a\": false\n}");
    /* Code point order: 'B' < 'a' < 'b' < U+00E9 < U+1F600. */
    expect_dump("{\"\\ud83d\\ude00\":1,\"b\":2,\"\xc3\xa9\":3,\"a\":4,\"B\":5}",
                1,
                "{\n  \"B\": 5,\n  \"a\": 4,\n  \"b\": 2,\n  \"\\u00e9\": 3,\n"
                "  \"\\ud83d\\ude00\": 1\n}");
    /* A key that is a prefix of another sorts first. */
    expect_dump("{\"ab\":1,\"a\":2}", 1, "{\n  \"a\": 2,\n  \"ab\": 1\n}");
}

static void test_python_strings(void)
{
    expect_dump("\"q\\\"b\\\\s/\\/\"", 0, "\"q\\\"b\\\\s//\"");
    expect_dump("\"\\b\\f\\n\\r\\t\\u0000\\u001f \\u007f~\"", 0,
                "\"\\b\\f\\n\\r\\t\\u0000\\u001f \\u007f~\"");
    expect_dump("\"\xc3\xa9\xe4\xb8\xad\xf0\x9f\x98\x80\"", 0,
                "\"\\u00e9\\u4e2d\\ud83d\\ude00\"");
    expect_dump("\"\\uD83D\\uDE00\"", 0, "\"\\ud83d\\ude00\"");
    /* Lone and reversed surrogates survive as Python keeps them. */
    expect_dump("\"\\ud800x\\udfff\\ude00\\ud83d\"", 0,
                "\"\\ud800x\\udfff\\ude00\\ud83d\"");
    expect_dump("\"\\u2028\\u00a0\"", 0, "\"\\u2028\\u00a0\"");
}

static void test_python_numbers(void)
{
    static const char *const cases[][2] = {
        { "0", "0" }, { "-0", "0" }, { "12", "12" }, { "-12", "-12" },
        { "123456789012345678901234567890", "123456789012345678901234567890" },
        { "1.0", "1.0" }, { "1.50", "1.5" }, { "-0.0", "-0.0" }, { "0e0", "0.0" },
        { "1E2", "100.0" }, { "1e16", "1e+16" }, { "1e15", "1000000000000000.0" },
        { "0.0001", "0.0001" }, { "0.00001", "1e-05" }, { "0.1", "0.1" },
        { "5e-324", "5e-324" }, { "1e-400", "0.0" },
        { "1.7976931348623157e308", "1.7976931348623157e+308" },
        { "2.2250738585072014e-308", "2.2250738585072014e-308" },
        { "8.98846567431158e307", "8.98846567431158e+307" },
        { "123456789012345678.0", "1.2345678901234568e+17" },
        { "9007199254740993.0", "9007199254740992.0" },
        { "1.5e300", "1.5e+300" }, { "-2.5e-7", "-2.5e-07" },
        { "0.30000000000000004", "0.30000000000000004" },
        { "1234.5678", "1234.5678" }, { "1e22", "1e+22" }, { "1e23", "1e+23" },
        /*
         * Powers of two (2**-1017, 2**-808) whose correctly rounded 16-digit
         * decimal does not round-trip, while the next decimal up does.
         */
        { "7.120236347223045e-307", "7.120236347223045e-307" },
        { "5.858190679279809e-244", "5.858190679279809e-244" },
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++)
        expect_dump(cases[index][0], 0, cases[index][1]);
}

static void test_mutation(void)
{
    struct json_error error;
    struct json_value *object = parse_text("{\"a\":1,\"b\":2,\"c\":3}", &error);
    CHECK(object != NULL);
    if (!object)
        return;
    struct json_value *taken = json_object_take(object, "b");
    CHECK(taken && strcmp(taken->text, "2") == 0 && object->count == 2);
    CHECK(json_object_take(object, "b") == NULL);
    json_free(taken);
    CHECK(json_object_set(object, "a", json_new_cstring("x")) == 0);
    CHECK(object->count == 2 && json_string_equals(json_object_get(object, "a"),
                                                   "x"));
    struct json_value *array = json_new_container(JSON_ARRAY);
    CHECK(json_array_append(array, json_new_integer(-7)) == 0);
    CHECK(json_array_append(array, json_new_literal(JSON_FALSE)) == 0);
    CHECK(json_object_set(object, "list", array) == 0);
    CHECK(json_array_append(object, json_new_literal(JSON_NULL)) != 0);
    struct json_buffer out = { 0 };
    CHECK(json_dump(object, 0, &out) == 0);
    CHECK(out.data && strcmp(out.data,
        "{\n  \"a\": \"x\",\n  \"c\": 3,\n  \"list\": [\n    -7,\n    false\n  ]\n}")
        == 0);
    json_buffer_free(&out);
    json_free(object);
    CHECK(json_new_literal(JSON_STRING) == NULL);
    CHECK(json_new_container(JSON_TRUE) == NULL);
}

static void test_utf8(void)
{
    CHECK(json_utf8_valid("", 0));
    CHECK(json_utf8_valid("a\xc3\xa9\xf0\x9f\x98\x80", 7));
    CHECK(!json_utf8_valid("\xc3", 1));
    CHECK(!json_utf8_valid("\xed\xbf\xbf", 3));
    CHECK(!json_utf8_valid("\xe0\x80\x80", 3));
    CHECK(!json_utf8_valid("\x80", 1));
}

int main(void)
{
    test_types_and_tokens();
    test_rejections();
    test_delete_character_is_valid();
    test_duplicates();
    test_limits();
    test_python_layout();
    test_python_strings();
    test_python_numbers();
    test_mutation();
    test_utf8();
    if (failures) {
        fprintf(stderr, "FAIL: %d strict JSON checks failed\n", failures);
        return 1;
    }
    printf("PASS: strict JSON parsing and Python-compatible output\n");
    return 0;
}
