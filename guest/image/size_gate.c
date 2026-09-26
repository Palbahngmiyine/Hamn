#include "image/size_gate.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include "image/digest.h"

#define PROPOSAL_SUFFIX ".budget-proposal.json"

struct inventory_name {
    const char *bytes;
    size_t length;
};

/*
 * Length of the line boundary at S, as Python's str.splitlines() defines
 * boundaries (\n, \r\n, \r, \v, \f, \x1c-\x1e, U+0085, U+2028, U+2029).
 */
static size_t line_boundary(const unsigned char *s, size_t n)
{
    switch (s[0]) {
    case '\n': case '\v': case '\f': case 0x1C: case 0x1D: case 0x1E:
        return 1;
    case '\r':
        return n > 1 && s[1] == '\n' ? 2 : 1;
    case 0xC2:
        return n > 1 && s[1] == 0x85 ? 2 : 0;
    case 0xE2:
        return n > 2 && s[1] == 0x80 && (s[2] == 0xA8 || s[2] == 0xA9) ? 3 : 0;
    default:
        return 0;
    }
}

/* Python's str.isspace() for one code point. */
static int unicode_space(unsigned code_point)
{
    return (code_point >= 0x09 && code_point <= 0x0D) ||
           (code_point >= 0x1C && code_point <= 0x20) || code_point == 0x85 ||
           code_point == 0xA0 || code_point == 0x1680 ||
           (code_point >= 0x2000 && code_point <= 0x200A) ||
           code_point == 0x2028 || code_point == 0x2029 ||
           code_point == 0x202F || code_point == 0x205F || code_point == 0x3000;
}

/* Decodes validated UTF-8 at S; returns the sequence length. */
static size_t next_code_point(const unsigned char *s, unsigned *code_point)
{
    if (s[0] < 0x80) {
        *code_point = s[0];
        return 1;
    }
    size_t length = s[0] >= 0xF0 ? 4 : s[0] >= 0xE0 ? 3 : 2;
    unsigned value = s[0] & (length == 2 ? 0x1F : length == 3 ? 0x0F : 0x07);
    for (size_t index = 1; index < length; index++)
        value = (value << 6) | (s[index] & 0x3F);
    *code_point = value;
    return length;
}

static int field_valid(const unsigned char *field, size_t length, int digits)
{
    if (length == 0)
        return 0;
    for (size_t offset = 0; offset < length;) {
        unsigned code_point;
        offset += next_code_point(field + offset, &code_point);
        if (unicode_space(code_point))
            return 0;
        if (digits && !(code_point >= '0' && code_point <= '9'))
            return 0;
    }
    return 1;
}

/* One "package TAB version TAB installed-KiB" row; stores the package name. */
static int row_valid(const unsigned char *row, size_t length,
                     struct inventory_name *name)
{
    const unsigned char *first = memchr(row, '\t', length);
    if (!first)
        return 0;
    size_t name_length = (size_t)(first - row);
    const unsigned char *version = first + 1;
    size_t rest = length - name_length - 1;
    const unsigned char *second = memchr(version, '\t', rest);
    if (!second)
        return 0;
    size_t version_length = (size_t)(second - version);
    const unsigned char *size = second + 1;
    size_t size_length = rest - version_length - 1;
    if (memchr(size, '\t', size_length))
        return 0;
    if (!field_valid(row, name_length, 0) ||
        !field_valid(version, version_length, 0) ||
        !field_valid(size, size_length, 1))
        return 0;
    name->bytes = (const char *)row;
    name->length = name_length;
    return 1;
}

static int name_order(const void *left, const void *right)
{
    const struct inventory_name *a = left, *b = right;
    size_t shorter = a->length < b->length ? a->length : b->length;
    int order = memcmp(a->bytes, b->bytes, shorter);
    return order ? order : (a->length > b->length) - (a->length < b->length);
}

int size_package_inventory(const char *path, struct json_value **rows,
                           struct image_error *error)
{
    char *text = NULL;
    size_t length = 0;
    if (image_read_file(path, IMAGE_JSON_MAX_BYTES, &text, &length, error) != 0)
        return -1;
    if (!json_utf8_valid(text, length)) {
        free(text);
        return image_fail(error, "invalid package inventory: %s", path);
    }
    /*
     * Python: Path.read_text() (universal newlines turn \r\n and \r into \n),
     * then .rstrip("\n").splitlines().
     */
    size_t translated = 0;
    for (size_t index = 0; index < length; index++) {
        if (text[index] == '\r') {
            text[translated++] = '\n';
            if (index + 1 < length && text[index + 1] == '\n')
                index++;
        } else {
            text[translated++] = text[index];
        }
    }
    length = translated;
    while (length > 0 && text[length - 1] == '\n')
        length--;
    struct json_value *array = json_new_container(JSON_ARRAY);
    struct inventory_name *names =
        malloc((length ? length : 1) * sizeof(*names));
    size_t count = 0;
    int invalid = 0, memory = !array || !names;
    const unsigned char *s = (const unsigned char *)text;
    size_t start = 0, offset = 0;
    while (!invalid && !memory && start <= length) {
        size_t boundary = 0;
        while (offset < length &&
               !(boundary = line_boundary(s + offset, length - offset)))
            offset++;
        if (offset == length && start == length)
            break;
        if (!row_valid(s + start, offset - start, &names[count])) {
            invalid = 1;
            break;
        }
        count++;
        struct json_value *row = json_new_string(text + start, offset - start);
        if (!row || json_array_append(array, row) != 0) {
            json_free(row);
            memory = 1;
            break;
        }
        if (offset == length)
            break;
        offset += boundary;
        start = offset;
    }
    if (!invalid && !memory && count > 1) {
        qsort(names, count, sizeof(*names), name_order);
        for (size_t index = 1; index < count; index++) {
            if (name_order(&names[index - 1], &names[index]) == 0)
                invalid = 1;
        }
    }
    free(names);
    free(text);
    if (memory) {
        json_free(array);
        return image_fail(error, "out of memory reading %s", path);
    }
    if (invalid) {
        json_free(array);
        return image_fail(error, "invalid package inventory: %s", path);
    }
    if (count == 0) {
        json_free(array);
        return image_fail(error, "empty package inventory: %s", path);
    }
    *rows = array;
    return 0;
}

int size_budget_validate(const struct json_value *budget,
                         struct image_error *error)
{
    long long schema = 0, maximum = 0;
    if (!budget || budget->type != JSON_OBJECT || budget->count != 4 ||
        json_integer_value(json_object_get(budget, "schemaVersion"),
                           &schema) != 0 || schema != 1 ||
        json_integer_value(json_object_get(budget, "maximumCompressedBytes"),
                           &maximum) != 0 ||
        maximum <= 0 || maximum >= IMAGE_RELEASE_ASSET_LIMIT ||
        !image_json_lower_hex(json_object_get(budget, "referenceImageSha256"),
                              64) ||
        !image_json_lower_hex(json_object_get(budget, "footprintReportSha256"),
                              64))
        return image_fail(error, "invalid reviewed size budget");
    return 0;
}

static int file_size(const char *path, long long *size,
                     struct image_error *error)
{
    struct stat info;
    if (stat(path, &info) != 0)
        return image_fail_errno(error, "cannot inspect", path);
    *size = (long long)info.st_size;
    return 0;
}

/* Regular file that is not itself a symlink (Python is_file, not is_symlink). */
static int regular_not_symlink(const char *path)
{
    struct stat info;
    if (lstat(path, &info) != 0 || S_ISLNK(info.st_mode))
        return 0;
    return stat(path, &info) == 0 && S_ISREG(info.st_mode);
}

static char *proposal_path(const char *report)
{
    const char *slash = strrchr(report, '/');
    const char *name = slash ? slash + 1 : report;
    const char *dot = strrchr(name, '.');
    /* Path.with_suffix: a leading or trailing dot is not a suffix. */
    size_t keep = dot && dot != name && dot[1] ? (size_t)(dot - report) :
                                                 strlen(report);
    char *path = malloc(keep + sizeof(PROPOSAL_SUFFIX));
    if (path) {
        memcpy(path, report, keep);
        memcpy(path + keep, PROPOSAL_SUFFIX, sizeof(PROPOSAL_SUFFIX));
    }
    return path;
}

static int add_member(struct json_value *object, const char *key,
                      struct json_value *value)
{
    if (!value)
        return -1;
    if (json_object_set(object, key, value) != 0) {
        json_free(value);
        return -1;
    }
    return 0;
}

static struct json_value *string_array(const char *const *items, size_t count)
{
    struct json_value *array = json_new_container(JSON_ARRAY);
    for (size_t index = 0; array && index < count; index++) {
        struct json_value *item = json_new_cstring(items[index]);
        if (!item || json_array_append(array, item) != 0) {
            json_free(item);
            json_free(array);
            return NULL;
        }
    }
    return array;
}

int size_gate_run(const struct size_gate_options *options,
                  struct image_error *error)
{
    static const char *const cleanup[] = {
        "build dependencies", "snapd and lxd-installer", "kernel device trees",
        "apt archives and lists", "temporary sources", "logs and journals",
        "cloud-init state", "machine-id", "SSH host keys", "systemd random seed",
        "duplicate files as hard links", "filesystem journal",
        "free filesystem blocks",
    };
    if (!image_lower_hex(options->base_sha256, strlen(options->base_sha256), 64) ||
        !image_lower_hex(options->source_revision,
                         strlen(options->source_revision), 40))
        return image_fail(error, "invalid source identity");
    long long baseline = 0, candidate = 0;
    if (file_size(options->baseline, &baseline, error) != 0 ||
        file_size(options->candidate, &candidate, error) != 0)
        return -1;
    if (baseline <= 0 || candidate <= 0)
        return image_fail(error, "empty baseline or candidate");
    long long required = image_required_savings(baseline);
    char baseline_sha[SHA256_HEX_CAP], image_sha[SHA256_HEX_CAP];
    if (sha256_file(options->baseline, baseline_sha, error) != 0 ||
        sha256_file(options->candidate, image_sha, error) != 0)
        return -1;
    struct json_value *before = NULL, *after = NULL;
    if (size_package_inventory(options->packages_before, &before, error) != 0)
        return -1;
    if (size_package_inventory(options->packages_after, &after, error) != 0) {
        json_free(before);
        return -1;
    }

    struct json_value *report = json_new_container(JSON_OBJECT);
    int built = report &&
        add_member(report, "schemaVersion", json_new_integer(1)) == 0 &&
        add_member(report, "baseImageSha256",
                   json_new_cstring(options->base_sha256)) == 0 &&
        add_member(report, "sourceRevision",
                   json_new_cstring(options->source_revision)) == 0 &&
        add_member(report, "virtualBytes",
                   json_new_integer(IMAGE_VIRTUAL_BYTES)) == 0 &&
        add_member(report, "baselineCompressedBytes",
                   json_new_integer(baseline)) == 0 &&
        add_member(report, "compressedBytes", json_new_integer(candidate)) == 0 &&
        add_member(report, "baselineSha256", json_new_cstring(baseline_sha)) == 0 &&
        add_member(report, "imageSha256", json_new_cstring(image_sha)) == 0 &&
        add_member(report, "savedBytes",
                   json_new_integer(baseline - candidate)) == 0 &&
        add_member(report, "requiredSavingsBytes",
                   json_new_integer(required)) == 0;
    if (built) {
        built = add_member(report, "packagesBefore", before) == 0;
        before = NULL;
    }
    if (built) {
        built = add_member(report, "packagesAfter", after) == 0;
        after = NULL;
    }
    built = built &&
        add_member(report, "cleanup",
                   string_array(cleanup, sizeof(cleanup) / sizeof(cleanup[0]))) == 0 &&
        add_member(report, "runtimeValidation",
                   json_new_cstring("required: physical boot, Docker, Compose, "
                                    "Buildx, binfmt, Rosetta, reboot")) == 0 &&
        add_member(report, "reviewOnly",
                   json_new_literal(options->review_only ? JSON_TRUE :
                                                           JSON_FALSE)) == 0;
    json_free(before);
    json_free(after);
    if (!built) {
        json_free(report);
        return image_fail(error, "out of memory building the size report");
    }
    int written = image_write_json(options->report, report, 0, error);
    json_free(report);
    if (written != 0)
        return -1;

    if (candidate >= IMAGE_RELEASE_ASSET_LIMIT)
        return image_fail(error,
            "compressed guest image exceeds the GitHub release asset limit");
    if (baseline - candidate < required)
        return image_fail(error, "image savings are below max(64 MiB, 5%%)");
    if (options->review_only) {
        char report_sha[SHA256_HEX_CAP];
        if (sha256_file(options->report, report_sha, error) != 0)
            return -1;
        struct json_value *proposal = json_new_container(JSON_OBJECT);
        int ok = proposal &&
            add_member(proposal, "schemaVersion", json_new_integer(1)) == 0 &&
            add_member(proposal, "maximumCompressedBytes",
                       json_new_integer(candidate)) == 0 &&
            add_member(proposal, "referenceImageSha256",
                       json_new_cstring(image_sha)) == 0 &&
            add_member(proposal, "footprintReportSha256",
                       json_new_cstring(report_sha)) == 0;
        char *path = ok ? proposal_path(options->report) : NULL;
        if (!path) {
            json_free(proposal);
            return image_fail(error, "out of memory building the budget proposal");
        }
        int rc = image_write_json(path, proposal, 0, error);
        free(path);
        json_free(proposal);
        return rc;
    }
    if (!regular_not_symlink(options->budget))
        return image_fail(error,
            "reviewed release-size-budget.json is required; use review-only to produce evidence");
    struct json_value *budget = image_read_json(options->budget, error);
    if (!budget)
        return -1;
    long long maximum = 0;
    int rc = size_budget_validate(budget, error);
    if (rc == 0) {
        json_integer_value(json_object_get(budget, "maximumCompressedBytes"),
                           &maximum);
        if (candidate > maximum)
            rc = image_fail(error,
                "guest image exceeds reviewed size budget; review the footprint report before changing it");
    }
    json_free(budget);
    return rc;
}

static int positive_integer(const struct json_value *report, const char *key,
                            long long *out)
{
    return json_integer_value(json_object_get(report, key), out) == 0 &&
           *out > 0;
}

static int nonempty_string_list(const struct json_value *value)
{
    if (!value || value->type != JSON_ARRAY || value->count == 0)
        return 0;
    for (size_t index = 0; index < value->count; index++) {
        const struct json_value *item = value->members[index].value;
        if (item->type != JSON_STRING || item->length == 0)
            return 0;
    }
    return 1;
}

int size_release_verify(const char *image, const char *report_path,
                        const char *budget_path, const char *expected_revision,
                        struct image_error *error)
{
    if (!regular_not_symlink(image) || !regular_not_symlink(report_path) ||
        !regular_not_symlink(budget_path))
        return image_fail(error,
            "regular image, size report and reviewed release-size-budget.json are required");
    struct json_value *report = image_read_json(report_path, error);
    if (!report)
        return -1;
    struct json_value *budget = image_read_json(budget_path, error);
    if (!budget) {
        json_free(report);
        return -1;
    }
    int rc = size_budget_validate(budget, error);
    long long schema = 0, compressed = 0, baseline = 0, saved = 0;
    long long required_reported = 0, virtual_bytes = 0, maximum = 0;
    const struct json_value *review = json_object_get(report, "reviewOnly");
    if (rc == 0 && (report->type != JSON_OBJECT ||
                    json_integer_value(json_object_get(report, "schemaVersion"),
                                       &schema) != 0 ||
                    schema != 1 || !review || review->type != JSON_FALSE))
        rc = image_fail(error, "review-only or invalid image report cannot be published");
    if (rc == 0 && !(positive_integer(report, "compressedBytes", &compressed) &&
                     positive_integer(report, "baselineCompressedBytes", &baseline) &&
                     positive_integer(report, "savedBytes", &saved) &&
                     positive_integer(report, "requiredSavingsBytes",
                                      &required_reported) &&
                     positive_integer(report, "virtualBytes", &virtual_bytes)))
        rc = image_fail(error, "invalid image report sizes");
    if (rc == 0 &&
        !(image_json_lower_hex(json_object_get(report, "imageSha256"), 64) &&
          image_json_lower_hex(json_object_get(report, "baselineSha256"), 64) &&
          image_json_lower_hex(json_object_get(report, "baseImageSha256"), 64)))
        rc = image_fail(error, "invalid image report digest");
    const struct json_value *revision = json_object_get(report, "sourceRevision");
    if (rc == 0 && !image_json_lower_hex(revision, 40))
        rc = image_fail(error, "invalid image source revision");
    if (rc == 0 && expected_revision &&
        !json_string_equals(revision, expected_revision))
        rc = image_fail(error,
            "guest image size report belongs to a different source revision");
    long long actual = 0;
    if (rc == 0)
        rc = file_size(image, &actual, error);
    if (rc == 0) {
        json_integer_value(json_object_get(budget, "maximumCompressedBytes"),
                           &maximum);
        long long required = image_required_savings(baseline);
        int matches = virtual_bytes == IMAGE_VIRTUAL_BYTES &&
                      compressed == actual &&
                      actual < IMAGE_RELEASE_ASSET_LIMIT && actual <= maximum &&
                      saved == baseline - actual &&
                      baseline - actual >= required &&
                      required_reported == required;
        char actual_sha[SHA256_HEX_CAP];
        if (matches) {
            if (sha256_file(image, actual_sha, error) != 0)
                rc = -1;
            else
                matches = json_string_equals(json_object_get(report, "imageSha256"),
                                             actual_sha);
        }
        if (rc == 0 && !matches)
            rc = image_fail(error,
                "image bytes, savings or reviewed budget do not match size evidence");
    }
    if (rc == 0 &&
        !(nonempty_string_list(json_object_get(report, "packagesBefore")) &&
          nonempty_string_list(json_object_get(report, "packagesAfter")) &&
          nonempty_string_list(json_object_get(report, "cleanup"))))
        rc = image_fail(error, "image footprint evidence is missing");
    json_free(report);
    json_free(budget);
    return rc;
}
