/*
 * Portable contract tests for hamn-image-tool's modules: baseline evidence
 * publication faults, size gates, the release publication boundary, GPT
 * checks and variation inputs. Synthetic sparse fixtures only; nothing here
 * measures real image savings or boots a VM.
 *
 * Expected digests and CPython random vectors were computed independently
 * (shasum, zlib, CPython 3) and are fixed here.
 */
#include <errno.h>
#include <fcntl.h>
#include <ftw.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <dirent.h>
#include <unistd.h>

#include "image/digest.h"
#include "image/evidence.h"
#include "image/raw_check.h"
#include "image/size_gate.h"
#include "image/variations.h"

#define MIB (1024LL * 1024)
#define GIB (1024LL * MIB)
#define BASELINE_BYTES (64 * MIB + 8192)
#define CANDIDATE_BYTES 8192
#define ZERO_BASELINE_SHA \
    "74c876a231458fadcf860c29732856bf68d6a5bec50cde416fd67fa60cd18511"
#define ZERO_CANDIDATE_SHA \
    "9f1dcbc35c350d6027f98be0f5c8b43b42ca52b7604459c0c42be3aa88913d47"
#define BASE_SHA "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
#define UPPER_BASE_SHA "Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
#define REVISION "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

static int failures;
static char root[4096];

#define CHECK(condition)                                                     \
    do {                                                                     \
        if (!(condition)) {                                                  \
            fprintf(stderr, "FAIL: %s:%d: %s\n", __FILE__, __LINE__,          \
                    #condition);                                             \
            failures++;                                                      \
        }                                                                    \
    } while (0)

/* The call fails and its reason contains TEXT. */
#define CHECK_FAILS(call, error, text)                                        \
    do {                                                                      \
        int check_rc = (call);                                                \
        if (check_rc == 0 || !strstr((error).message, (text))) {              \
            fprintf(stderr, "FAIL: %s:%d: %s -> rc %d, reason \"%s\", "        \
                    "expected \"%s\"\n", __FILE__, __LINE__, #call, check_rc,  \
                    check_rc ? (error).message : "", (text));                 \
            failures++;                                                       \
        }                                                                     \
    } while (0)

static char *path_of(const char *format, ...)
    __attribute__((format(printf, 1, 2)));

static char *path_of(const char *format, ...)
{
    static char paths[16][4096];
    static unsigned next;
    char *path = paths[next++ % 16];
    int used = snprintf(path, 4096, "%s/", root);
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(path + used, 4096 - (size_t)used, format, arguments);
    va_end(arguments);
    return path;
}

static void write_bytes(const char *path, const void *bytes, size_t length)
{
    FILE *file = fopen(path, "wb");
    if (!file || fwrite(bytes, 1, length, file) != length || fclose(file) != 0) {
        fprintf(stderr, "FATAL: cannot write fixture %s: %s\n", path,
                strerror(errno));
        exit(2);
    }
}

static void write_text(const char *path, const char *text)
{
    write_bytes(path, text, strlen(text));
}

static char *read_all(const char *path, size_t *length)
{
    FILE *file = fopen(path, "rb");
    if (!file)
        return NULL;
    size_t capacity = 4096, used = 0;
    char *data = malloc(capacity + 1);
    size_t count;
    while (data && (count = fread(data + used, 1, capacity - used, file)) > 0) {
        used += count;
        if (used == capacity) {
            char *grown = realloc(data, capacity * 2 + 1);
            if (!grown) {
                free(data);
                data = NULL;
                break;
            }
            data = grown;
            capacity *= 2;
        }
    }
    fclose(file);
    if (data)
        data[used] = '\0';
    if (length)
        *length = used;
    return data;
}

static void sparse(const char *path, long long size)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0 || ftruncate(fd, (off_t)size) != 0 || close(fd) != 0) {
        fprintf(stderr, "FATAL: cannot size fixture %s: %s\n", path,
                strerror(errno));
        exit(2);
    }
}

static int exists(const char *path)
{
    struct stat info;
    return lstat(path, &info) == 0;
}

static int remove_entry(const char *path, const struct stat *info, int type,
                        struct FTW *walk)
{
    (void)info;
    (void)walk;
    return type == FTW_DP ? rmdir(path) : unlink(path);
}

static void reset_root(void)
{
    if (root[0])
        nftw(root, remove_entry, 16, FTW_DEPTH | FTW_PHYS);
    const char *tmp = getenv("TMPDIR");
    snprintf(root, sizeof(root), "%s/hamn-image-tool-test.XXXXXX",
             tmp && *tmp ? tmp : "/tmp");
    if (!mkdtemp(root)) {
        fprintf(stderr, "FATAL: cannot create test root: %s\n", strerror(errno));
        exit(2);
    }
}

static void digest_text(const void *bytes, size_t length,
                        char hex[SHA256_HEX_CAP])
{
    struct sha256 context;
    unsigned char digest[SHA256_DIGEST_BYTES];
    sha256_init(&context);
    sha256_update(&context, bytes, length);
    sha256_final(&context, digest);
    sha256_hex(digest, hex);
}

/* ---- digests ---------------------------------------------------------- */

static void test_digest_vectors(void)
{
    char hex[SHA256_HEX_CAP];
    digest_text("", 0, hex);
    CHECK(strcmp(hex, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855") == 0);
    digest_text("abc", 3, hex);
    CHECK(strcmp(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad") == 0);
    const char *two_blocks = "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    digest_text(two_blocks, strlen(two_blocks), hex);
    CHECK(strcmp(hex, "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1") == 0);
    /* One million 'a' in uneven chunks exercises block carry-over. */
    struct sha256 context;
    unsigned char digest[SHA256_DIGEST_BYTES];
    char chunk[997];
    memset(chunk, 'a', sizeof(chunk));
    sha256_init(&context);
    size_t left = 1000000;
    while (left) {
        size_t take = left < sizeof(chunk) ? left : sizeof(chunk);
        sha256_update(&context, chunk, take);
        left -= take;
    }
    sha256_final(&context, digest);
    sha256_hex(digest, hex);
    CHECK(strcmp(hex, "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0") == 0);
    CHECK(crc32_ieee(0, "123456789", 9) == 0xCBF43926u);
    CHECK(crc32_ieee(crc32_ieee(0, "1234", 4), "56789", 5) == 0xCBF43926u);
    CHECK(crc32_ieee(0, "", 0) == 0);

    reset_root();
    sparse(path_of("zeros"), CANDIDATE_BYTES);
    struct image_error error;
    CHECK(sha256_file(path_of("zeros"), hex, &error) == 0 &&
          strcmp(hex, ZERO_CANDIDATE_SHA) == 0);
    CHECK_FAILS(sha256_file(path_of("missing"), hex, &error), error,
                "cannot open");
}

/* ---- baseline evidence -------------------------------------------------- */

struct fault {
    int fail_sync_at;
    int fail_link_at;
    int syncs;
    int links;
    const char *race_name;
    const char *race_path;
};

static int fault_sync(int fd, void *context)
{
    struct fault *fault = context;
    if (++fault->syncs == fault->fail_sync_at) {
        errno = EIO;
        return -1;
    }
    return fsync(fd);
}

static int fault_link(const char *source, int directory_fd, const char *name,
                      void *context)
{
    struct fault *fault = context;
    if (++fault->links == fault->fail_link_at) {
        errno = EIO;
        return -1;
    }
    if (fault->race_name && strcmp(name, fault->race_name) == 0)
        write_text(fault->race_path, "competing evidence");
    return linkat(AT_FDCWD, source, directory_fd, name, 0);
}

static unsigned char evidence_data[256 * 19];

static int stage_directories(void)
{
    DIR *directory = opendir(root);
    int count = 0;
    struct dirent *entry;
    while (directory && (entry = readdir(directory)))
        count += strncmp(entry->d_name, ".hamn-baseline-", 15) == 0;
    if (directory)
        closedir(directory);
    return count;
}

static void evidence_fixture(void)
{
    reset_root();
    for (size_t index = 0; index < sizeof(evidence_data); index++)
        evidence_data[index] = (unsigned char)(index % 256);
    write_bytes(path_of("source.img"), evidence_data, sizeof(evidence_data));
    char hex[SHA256_HEX_CAP], report[256];
    digest_text(evidence_data, sizeof(evidence_data), hex);
    snprintf(report, sizeof(report),
             "{\"baselineSha256\": \"%s\", \"baselineCompressedBytes\": %zu}",
             hex, sizeof(evidence_data));
    write_text(path_of("report.json"), report);
}

static void assert_no_partial(void)
{
    CHECK(!exists(path_of("baseline.img")));
    CHECK(!exists(path_of("baseline.img.sha256")));
    CHECK(stage_directories() == 0);
    size_t length = 0;
    char *source = read_all(path_of("source.img"), &length);
    CHECK(source && length == sizeof(evidence_data) &&
          memcmp(source, evidence_data, length) == 0);
    free(source);
    struct stat info;
    CHECK(lstat(path_of("source.img"), &info) == 0 && info.st_nlink == 1);
}

static int publish(const struct evidence_io *io, struct image_error *error)
{
    return evidence_publish(path_of("source.img"), path_of("baseline.img"),
                            path_of("report.json"), io, error);
}

static void test_evidence_publication(void)
{
    struct image_error error;
    evidence_fixture();
    CHECK(publish(NULL, &error) == 0);
    size_t length = 0;
    char *published = read_all(path_of("baseline.img"), &length);
    CHECK(published && length == sizeof(evidence_data) &&
          memcmp(published, evidence_data, length) == 0);
    free(published);
    char hex[SHA256_HEX_CAP], expected[128];
    digest_text(evidence_data, sizeof(evidence_data), hex);
    snprintf(expected, sizeof(expected), "%s  baseline.img\n", hex);
    char *sidecar = read_all(path_of("baseline.img.sha256"), NULL);
    CHECK(sidecar && strcmp(sidecar, expected) == 0);
    free(sidecar);
    const char *outputs[] = { "baseline.img", "baseline.img.sha256" };
    for (size_t index = 0; index < 2; index++) {
        struct stat info;
        CHECK(lstat(path_of("%s", outputs[index]), &info) == 0 &&
              (info.st_mode & 07777) == 0600 && info.st_nlink == 1);
    }
    CHECK(stage_directories() == 0);
    CHECK_FAILS(publish(NULL, &error), error, "collides");
    published = read_all(path_of("baseline.img"), &length);
    CHECK(published && length == sizeof(evidence_data) &&
          memcmp(published, evidence_data, length) == 0);
    free(published);
}

static void test_evidence_paths(void)
{
    struct image_error error;
    evidence_fixture();
    char *reserved[2] = { path_of("baseline.img.sha256"), NULL };
    CHECK_FAILS(evidence_check(path_of("baseline.img"), reserved, 1, &error),
                error, "collides");
    /* A reserved spelling through a symlinked directory is the same path. */
    CHECK(symlink(root, path_of("alias")) == 0);
    reserved[0] = path_of("alias/baseline.img");
    CHECK_FAILS(evidence_check(path_of("baseline.img"), reserved, 1, &error),
                error, "collides");
    reserved[0] = path_of("alias/missing-dir/../baseline.img");
    CHECK_FAILS(evidence_check(path_of("baseline.img"), reserved, 1, &error),
                error, "collides");
    reserved[0] = path_of("other.img");
    CHECK(evidence_check(path_of("baseline.img"), reserved, 1, &error) == 0);
    CHECK(evidence_check(path_of("baseline.img"), NULL, 0, &error) == 0);

    CHECK(symlink(path_of("source.img"), path_of("baseline.img")) == 0);
    CHECK_FAILS(evidence_check(path_of("baseline.img"), NULL, 0, &error), error,
                "collides");
    CHECK(unlink(path_of("baseline.img")) == 0);
    write_text(path_of("baseline.img.sha256"), "stale");
    CHECK_FAILS(evidence_check(path_of("baseline.img"), NULL, 0, &error), error,
                "collides");
    CHECK(unlink(path_of("baseline.img.sha256")) == 0);

    CHECK(mkdir(path_of("unsafe"), 0700) == 0 &&
          chmod(path_of("unsafe"), 0777) == 0);
    CHECK_FAILS(evidence_check(path_of("unsafe/baseline"), NULL, 0, &error),
                error, "unsafe baseline output directory");
    CHECK(chmod(path_of("unsafe"), 0720) == 0);
    CHECK_FAILS(evidence_check(path_of("unsafe/baseline"), NULL, 0, &error),
                error, "unsafe baseline output directory");
    CHECK(chmod(path_of("unsafe"), 0700) == 0);
    CHECK(evidence_check(path_of("unsafe/baseline"), NULL, 0, &error) == 0);
    CHECK(symlink(path_of("unsafe"), path_of("linked-parent")) == 0);
    CHECK_FAILS(evidence_check(path_of("linked-parent/baseline"), NULL, 0,
                               &error), error, "unsafe baseline output directory");
    CHECK_FAILS(evidence_check(path_of("missing/baseline"), NULL, 0, &error),
                error, "cannot inspect baseline output directory");
    CHECK_FAILS(evidence_check(path_of("bad\x01name"), NULL, 0, &error), error,
                "invalid baseline output name");
    assert_no_partial();
}

static void write_report(const char *digest, const char *size)
{
    char report[256];
    snprintf(report, sizeof(report),
             "{\"baselineSha256\":\"%s\",\"baselineCompressedBytes\":%s}",
             digest, size);
    write_text(path_of("report.json"), report);
}

static void test_evidence_rejects_mismatched_sources(void)
{
    struct image_error error;
    evidence_fixture();
    char hex[SHA256_HEX_CAP], size[32];
    digest_text(evidence_data, sizeof(evidence_data), hex);
    snprintf(size, sizeof(size), "%zu", sizeof(evidence_data));
    char smaller[32], as_float[32];
    snprintf(smaller, sizeof(smaller), "%zu", sizeof(evidence_data) - 1);
    snprintf(as_float, sizeof(as_float), "%zu.0", sizeof(evidence_data));
    write_report("0000000000000000000000000000000000000000000000000000000000000000",
                 size);
    CHECK_FAILS(publish(NULL, &error), error, "digest differs");
    assert_no_partial();
    write_report(hex, smaller);
    CHECK_FAILS(publish(NULL, &error), error, "size differs");
    assert_no_partial();
    write_report(hex, as_float);
    CHECK_FAILS(publish(NULL, &error), error, "size differs");
    assert_no_partial();
    write_text(path_of("report.json"), "[]");
    CHECK_FAILS(publish(NULL, &error), error, "not a JSON object");
    write_text(path_of("report.json"), "{\"baselineCompressedBytes\":1}");
    CHECK_FAILS(publish(NULL, &error), error, "lacks baselineSha256");
    write_text(path_of("report.json"), "{\"a\":1,\"a\":1}");
    CHECK_FAILS(publish(NULL, &error), error, "duplicate key");
    assert_no_partial();

    write_report(hex, size);
    CHECK(link(path_of("source.img"), path_of("alias")) == 0);
    CHECK_FAILS(publish(NULL, &error), error, "unsafe baseline evidence source");
    CHECK(unlink(path_of("alias")) == 0);
    assert_no_partial();
    CHECK(chmod(path_of("report.json"), 0664) == 0);
    CHECK_FAILS(publish(NULL, &error), error, "unsafe baseline evidence source");
    CHECK(chmod(path_of("report.json"), 0644) == 0);
    CHECK(symlink(path_of("source.img"), path_of("linked-source")) == 0);
    CHECK_FAILS(evidence_publish(path_of("linked-source"),
                                 path_of("baseline.img"),
                                 path_of("report.json"), NULL, &error),
                error, "unsafe baseline evidence source");
    assert_no_partial();
    CHECK(publish(NULL, &error) == 0);
}

static void test_evidence_faults_roll_back(void)
{
    struct image_error error;
    evidence_fixture();
    struct { int sync_at; int link_at; } cases[] = {
        { 1, 0 }, { 2, 0 }, { 3, 0 }, { 0, 1 }, { 0, 2 },
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        struct fault fault = { .fail_sync_at = cases[index].sync_at,
                               .fail_link_at = cases[index].link_at };
        struct evidence_io io = { fault_sync, fault_link, &fault };
        CHECK_FAILS(publish(&io, &error), error, strerror(EIO));
        assert_no_partial();
    }
    /* Every step ran: stage, sidecar and directory fsync, two links. */
    struct fault counted = { 0 };
    struct evidence_io io = { fault_sync, fault_link, &counted };
    CHECK(publish(&io, &error) == 0);
    CHECK(counted.syncs == 3 && counted.links == 2);
}

static void test_evidence_race_preserves_competitor(void)
{
    struct image_error error;
    evidence_fixture();
    struct fault fault = { .race_name = "baseline.img",
                           .race_path = path_of("baseline.img") };
    struct evidence_io io = { fault_sync, fault_link, &fault };
    CHECK_FAILS(publish(&io, &error), error, strerror(EEXIST));
    char *competitor = read_all(path_of("baseline.img"), NULL);
    CHECK(competitor && strcmp(competitor, "competing evidence") == 0);
    free(competitor);
    CHECK(!exists(path_of("baseline.img.sha256")));
    CHECK(stage_directories() == 0);
}

/* ---- size gate ---------------------------------------------------------- */

static struct size_gate_options gate_options(int review_only)
{
    struct size_gate_options options = {
        .baseline = path_of("baseline"),
        .candidate = path_of("candidate"),
        .packages_before = path_of("packages.tsv"),
        .packages_after = path_of("packages.tsv"),
        .report = path_of("report.json"),
        .budget = path_of("budget.json"),
        .base_sha256 = BASE_SHA,
        .source_revision = REVISION,
        .review_only = review_only,
    };
    return options;
}

static int run_gate(int review_only, struct image_error *error)
{
    struct size_gate_options options = gate_options(review_only);
    return size_gate_run(&options, error);
}

static void size_fixture(void)
{
    reset_root();
    sparse(path_of("baseline"), BASELINE_BYTES);
    sparse(path_of("candidate"), CANDIDATE_BYTES);
    write_text(path_of("packages.tsv"), "docker.io\t28.0\t12345\n");
}

static struct json_value *load_json(const char *path)
{
    struct image_error error;
    struct json_value *value = image_read_json(path, &error);
    CHECK(value != NULL);
    return value;
}

static void copy_file(const char *from, const char *to)
{
    size_t length = 0;
    char *data = read_all(from, &length);
    CHECK(data != NULL);
    if (data)
        write_bytes(to, data, length);
    free(data);
}

static void test_size_report_layout_and_budget_flow(void)
{
    struct image_error error;
    size_fixture();
    CHECK_FAILS(run_gate(0, &error), error, "reviewed release-size-budget");
    CHECK(exists(path_of("report.json")) && !exists(path_of("budget.json")));
    CHECK(run_gate(1, &error) == 0);
    char expected[4096];
    snprintf(expected, sizeof(expected),
        "{\n  \"schemaVersion\": 1,\n  \"baseImageSha256\": \"%s\",\n"
        "  \"sourceRevision\": \"%s\",\n  \"virtualBytes\": 8589934592,\n"
        "  \"baselineCompressedBytes\": %lld,\n  \"compressedBytes\": %d,\n"
        "  \"baselineSha256\": \"%s\",\n  \"imageSha256\": \"%s\",\n"
        "  \"savedBytes\": %lld,\n  \"requiredSavingsBytes\": %lld,\n"
        "  \"packagesBefore\": [\n    \"docker.io\\t28.0\\t12345\"\n  ],\n"
        "  \"packagesAfter\": [\n    \"docker.io\\t28.0\\t12345\"\n  ],\n"
        "  \"cleanup\": [\n    \"build dependencies\",\n"
        "    \"snapd and lxd-installer\",\n    \"kernel device trees\",\n"
        "    \"apt archives and lists\",\n    \"temporary sources\",\n"
        "    \"logs and journals\",\n    \"cloud-init state\",\n"
        "    \"machine-id\",\n    \"SSH host keys\",\n"
        "    \"systemd random seed\",\n"
        "    \"duplicate files as hard links\",\n    \"filesystem journal\",\n"
        "    \"free filesystem blocks\"\n  ],\n"
        "  \"runtimeValidation\": \"required: physical boot, Docker, Compose, "
        "Buildx, binfmt, Rosetta, reboot\",\n  \"reviewOnly\": true\n}\n",
        BASE_SHA, REVISION, BASELINE_BYTES, CANDIDATE_BYTES, ZERO_BASELINE_SHA,
        ZERO_CANDIDATE_SHA, BASELINE_BYTES - CANDIDATE_BYTES, 64 * MIB);
    size_t report_length = 0;
    char *report = read_all(path_of("report.json"), &report_length);
    CHECK(report && strcmp(report, expected) == 0);
    char report_sha[SHA256_HEX_CAP];
    digest_text(report ? report : "", report_length, report_sha);
    free(report);
    char proposal_expected[512];
    snprintf(proposal_expected, sizeof(proposal_expected),
             "{\n  \"schemaVersion\": 1,\n  \"maximumCompressedBytes\": %d,\n"
             "  \"referenceImageSha256\": \"%s\",\n"
             "  \"footprintReportSha256\": \"%s\"\n}\n",
             CANDIDATE_BYTES, ZERO_CANDIDATE_SHA, report_sha);
    char *proposal = read_all(path_of("report.budget-proposal.json"), NULL);
    CHECK(proposal && strcmp(proposal, proposal_expected) == 0);
    free(proposal);
    CHECK(!exists(path_of("budget.json")));
    copy_file(path_of("report.budget-proposal.json"), path_of("budget.json"));
    CHECK(run_gate(0, &error) == 0);
    struct json_value *value = load_json(path_of("report.json"));
    CHECK(value && json_object_get(value, "reviewOnly")->type == JSON_FALSE);
    json_free(value);
}

static void test_size_limits_cannot_be_bypassed(void)
{
    struct image_error error;
    CHECK(image_required_savings(1) == 64 * MIB);
    CHECK(image_required_savings(20 * 64 * MIB) == 64 * MIB);
    CHECK(image_required_savings(20 * 64 * MIB + 1) == 64 * MIB + 1);
    CHECK(image_required_savings(2 * GIB) == 107374183);
    size_fixture();
    /* Exactly the required savings is accepted (the fixture); one byte less is not. */
    sparse(path_of("candidate"), CANDIDATE_BYTES + 1);
    CHECK_FAILS(run_gate(1, &error), error, "savings");
    CHECK(exists(path_of("report.json")));
    CHECK(!exists(path_of("report.budget-proposal.json")));
    sparse(path_of("candidate"), 2 * GIB);
    sparse(path_of("baseline"), 3 * GIB);
    CHECK_FAILS(run_gate(1, &error), error, "asset limit");
    CHECK(!exists(path_of("report.budget-proposal.json")));
    sparse(path_of("candidate"), 0);
    CHECK_FAILS(run_gate(1, &error), error, "empty baseline or candidate");
    sparse(path_of("baseline"), BASELINE_BYTES);
    sparse(path_of("candidate"), CANDIDATE_BYTES);
    char upper[] = BASE_SHA;
    upper[0] = 'A';
    struct size_gate_options options = gate_options(1);
    options.base_sha256 = upper;
    CHECK_FAILS(size_gate_run(&options, &error), error, "invalid source identity");
    options = gate_options(1);
    options.source_revision = &REVISION[1];
    CHECK_FAILS(size_gate_run(&options, &error), error, "invalid source identity");
    options = gate_options(1);
    options.candidate = path_of("missing");
    CHECK_FAILS(size_gate_run(&options, &error), error, "cannot inspect");
}

static void test_size_budget_rejections(void)
{
    struct image_error error;
    size_fixture();
    CHECK(run_gate(1, &error) == 0);
    struct json_value *proposal = load_json(path_of("report.budget-proposal.json"));
    if (!proposal)
        return;
    CHECK(json_object_set(proposal, "maximumCompressedBytes",
                          json_new_integer(CANDIDATE_BYTES - 1)) == 0);
    CHECK(image_write_json(path_of("budget.json"), proposal, 0, &error) == 0);
    CHECK_FAILS(run_gate(0, &error), error, "exceeds reviewed");
    CHECK(json_object_set(proposal, "unknown", json_new_literal(JSON_TRUE)) == 0);
    CHECK(image_write_json(path_of("budget.json"), proposal, 0, &error) == 0);
    CHECK_FAILS(run_gate(0, &error), error, "invalid reviewed");
    json_free(proposal);
    write_text(path_of("budget.json"), "{\"schemaVersion\":1,\"schemaVersion\":1}");
    CHECK_FAILS(run_gate(0, &error), error, "duplicate");
    write_text(path_of("budget.json"), "{\"schemaVersion\":NaN}");
    CHECK_FAILS(run_gate(0, &error), error, "invalid JSON");
    CHECK(unlink(path_of("budget.json")) == 0);
    CHECK(symlink(path_of("report.budget-proposal.json"),
                  path_of("budget.json")) == 0);
    CHECK_FAILS(run_gate(0, &error), error, "reviewed release-size-budget");
}

static void test_budget_schema(void)
{
    static const char *const invalid[] = {
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":true,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":2147483648,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":0,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":-1,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":1.5,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":1.0,\"maximumCompressedBytes\":100,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":true,\"maximumCompressedBytes\":100,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":2,\"maximumCompressedBytes\":100,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":100,\"referenceImageSha256\":\"%s\"}",
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":100,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\",\"x\":1}",
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":100,\"referenceImageSha256\":\"\",\"footprintReportSha256\":\"%s\"}",
        "{\"schemaVersion\":1,\"maximumCompressedBytes\":100,\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}",
        "[\"%s\",\"%s\"]",
    };
    const char *a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const char *b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    struct image_error error;
    char text[512];
    snprintf(text, sizeof(text),
             "{\"schemaVersion\":1,\"maximumCompressedBytes\":2147483647,"
             "\"referenceImageSha256\":\"%s\",\"footprintReportSha256\":\"%s\"}",
             a, b);
    struct json_value *valid = json_parse(text, strlen(text), NULL);
    CHECK(valid && size_budget_validate(valid, &error) == 0);
    json_free(valid);
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        snprintf(text, sizeof(text), invalid[index], a, b);
        struct json_value *budget = json_parse(text, strlen(text), NULL);
        CHECK(budget != NULL);
        CHECK_FAILS(size_budget_validate(budget, &error), error,
                    "invalid reviewed size budget");
        json_free(budget);
    }
}

static int release(const char *revision, struct image_error *error)
{
    return size_release_verify(path_of("candidate"), path_of("report.json"),
                               path_of("budget.json"), revision, error);
}

static void test_release_boundary(void)
{
    struct image_error error;
    size_fixture();
    CHECK(run_gate(1, &error) == 0);
    copy_file(path_of("report.budget-proposal.json"), path_of("budget.json"));
    CHECK_FAILS(release(NULL, &error), error, "review-only");
    CHECK(run_gate(0, &error) == 0);
    CHECK(release(NULL, &error) == 0);
    CHECK(release(REVISION, &error) == 0);
    CHECK_FAILS(release("cccccccccccccccccccccccccccccccccccccccc", &error),
                error, "different source revision");

    /* Each report field is checked independently of the others. */
    struct { const char *key; struct json_value *value; const char *reason; } cases[] = {
        { "reviewOnly", json_new_literal(JSON_TRUE), "review-only" },
        { "reviewOnly", json_new_literal(JSON_NULL), "review-only" },
        { "schemaVersion", json_new_integer(2), "review-only" },
        { "compressedBytes", json_new_cstring("8192"), "invalid image report sizes" },
        { "savedBytes", json_new_integer(0), "invalid image report sizes" },
        { "virtualBytes", json_new_literal(JSON_TRUE), "invalid image report sizes" },
        { "imageSha256", json_new_cstring(&ZERO_CANDIDATE_SHA[1]), "invalid image report digest" },
        { "baseImageSha256", json_new_cstring(UPPER_BASE_SHA), "invalid image report digest" },
        { "sourceRevision", json_new_cstring(REVISION "0"), "invalid image source revision" },
        { "virtualBytes", json_new_integer(4 * GIB), "do not match" },
        { "compressedBytes", json_new_integer(CANDIDATE_BYTES + 1), "do not match" },
        { "requiredSavingsBytes", json_new_integer(64 * MIB + 1), "do not match" },
        { "savedBytes", json_new_integer(64 * MIB - 1), "do not match" },
        { "packagesBefore", json_new_container(JSON_ARRAY), "footprint evidence" },
        { "cleanup", json_new_cstring("x"), "footprint evidence" },
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        struct json_value *copy = load_json(path_of("report.json"));
        CHECK(copy && json_object_set(copy, cases[index].key,
                                      cases[index].value) == 0);
        CHECK(image_write_json(path_of("mutated.json"), copy, 0, &error) == 0);
        CHECK_FAILS(size_release_verify(path_of("candidate"),
                                        path_of("mutated.json"),
                                        path_of("budget.json"), NULL, &error),
                    error, cases[index].reason);
        json_free(copy);
    }
    struct json_value *lists = load_json(path_of("report.json"));
    struct json_value *empty_item = json_new_container(JSON_ARRAY);
    CHECK(json_array_append(empty_item, json_new_cstring("")) == 0);
    CHECK(lists && json_object_set(lists, "packagesAfter", empty_item) == 0);
    CHECK(image_write_json(path_of("mutated.json"), lists, 0, &error) == 0);
    CHECK_FAILS(size_release_verify(path_of("candidate"), path_of("mutated.json"),
                                    path_of("budget.json"), NULL, &error),
                error, "footprint evidence");
    json_free(lists);

    CHECK(symlink(path_of("report.json"), path_of("linked-report.json")) == 0);
    CHECK_FAILS(size_release_verify(path_of("candidate"),
                                    path_of("linked-report.json"),
                                    path_of("budget.json"), NULL, &error),
                error, "regular image, size report");
    CHECK_FAILS(size_release_verify(path_of("candidate"), path_of("report.json"),
                                    path_of("absent.json"), NULL, &error),
                error, "regular image, size report");
    /* The budget itself is revalidated at publication. */
    write_text(path_of("bad-budget.json"), "{\"schemaVersion\":1}");
    CHECK_FAILS(size_release_verify(path_of("candidate"), path_of("report.json"),
                                    path_of("bad-budget.json"), NULL, &error),
                error, "invalid reviewed size budget");

    int fd = open(path_of("candidate"), O_WRONLY);
    CHECK(fd >= 0 && write(fd, "changed", 7) == 7 && close(fd) == 0);
    CHECK_FAILS(release(NULL, &error), error, "do not match");
}

static void test_inventory_newlines_and_pinning(void)
{
    struct image_error error;
    size_fixture();
    /* guestfish prints its own newline after dpkg-query's final newline. */
    write_text(path_of("packages.tsv"),
               "docker.io\t28.0\t12345\nrunc\t1.3.4\t34734\n\n");
    CHECK(run_gate(1, &error) == 0);
    struct json_value *report = load_json(path_of("report.json"));
    const char *rows[] = { "docker.io\t28.0\t12345", "runc\t1.3.4\t34734" };
    const struct json_value *after = report ?
        json_object_get(report, "packagesAfter") : NULL;
    CHECK(after && after->count == 2 &&
          json_string_equals(after->members[0].value, rows[0]) &&
          json_string_equals(after->members[1].value, rows[1]));
    CHECK(report && variations_verify_inventory(path_of("packages.tsv"), after,
                                                &error) == 0);
    write_text(path_of("changed.tsv"),
               "docker.io\t29.0\t12345\nrunc\t1.3.4\t34734\n\n");
    CHECK_FAILS(variations_verify_inventory(path_of("changed.tsv"), after, &error),
                error, "changed the pinned");
    write_text(path_of("changed.tsv"), "docker.io\t28.0\t12345\n");
    CHECK_FAILS(variations_verify_inventory(path_of("changed.tsv"), after, &error),
                error, "changed the pinned");
    json_free(report);
    copy_file(path_of("report.budget-proposal.json"), path_of("budget.json"));
    CHECK(run_gate(0, &error) == 0);
    CHECK(release(NULL, &error) == 0);

    /* Python read_text() universal newlines: CR and CRLF end rows. */
    struct json_value *crlf = NULL;
    write_text(path_of("crlf.tsv"), "a\tb\t1\r\nc\td\t2\re\tf\t3\r\r");
    CHECK(size_package_inventory(path_of("crlf.tsv"), &crlf, &error) == 0 &&
          crlf && crlf->count == 3 &&
          json_string_equals(crlf->members[2].value, "e\tf\t3"));
    json_free(crlf);
}

static void test_invalid_inventories_never_publish_reports(void)
{
    static const char *const invalid[] = {
        "", "\n\n", "docker.io\t28.0\t12345\n\nrunc\t1.3.4\t34734\n",
        "docker.io\t28.0\n", "docker.io\t28.0\t-1\n", "docker.io\t\t12345\n",
        "docker.io\t28.0\t12345\ndocker.io\t28.0\t12345\n",
        "docker io\t28.0\t1\n", "a\tb\t1\tc\n", "a\tb\t1x\n", "a\tb\t\n",
        "\tb\t1\n", "a\tb\xc2\xa0\t1\n", "\xff\tb\t1\n",
        "a\tb\t\xd9\xa1\n", "a\tb\t1\na\tc\t2\n",
    };
    struct image_error error;
    size_fixture();
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        write_text(path_of("packages.tsv"), invalid[index]);
        CHECK_FAILS(run_gate(1, &error), error, "package inventory");
        CHECK(!exists(path_of("report.json")));
    }
    /* A vertical tab is a Python line boundary, not part of a field. */
    struct json_value *rows = NULL;
    write_text(path_of("vt.tsv"), "a\tb\t1\x0b" "c\td\t2\n");
    CHECK(size_package_inventory(path_of("vt.tsv"), &rows, &error) == 0 &&
          rows && rows->count == 2);
    json_free(rows);
}

/* ---- raw GPT checks ----------------------------------------------------- */

struct gpt_fixture {
    uint64_t table_lba;
    uint32_t entries;
    uint32_t entry_size;
    uint32_t header_size;
    int empty_table;
    int bad_signature;
    int bad_header_crc;
    int bad_table_crc;
    int no_boot_signature;
    int wrong_type;
    long long size;
};

static void put32(unsigned char *out, uint32_t value)
{
    for (int index = 0; index < 4; index++)
        out[index] = (unsigned char)(value >> (8 * index));
}

static void put64(unsigned char *out, uint64_t value)
{
    put32(out, (uint32_t)value);
    put32(out + 4, (uint32_t)(value >> 32));
}

static void write_gpt(const char *path, const struct gpt_fixture *gpt)
{
    sparse(path, gpt->size ? gpt->size : 8 * GIB);
    size_t table_size = (size_t)gpt->entries * gpt->entry_size;
    unsigned char *table = calloc(1, table_size ? table_size : 1);
    if (!gpt->empty_table && table_size)
        table[0] = 1;
    unsigned char mbr[512] = { 0 }, header[512] = { 0 };
    mbr[450] = gpt->wrong_type ? 0x83 : 0xEE;
    mbr[510] = 0x55;
    mbr[511] = gpt->no_boot_signature ? 0x00 : 0xAA;
    memcpy(header, gpt->bad_signature ? "EFI PARX" : "EFI PART", 8);
    put32(header + 8, 0x10000);
    put32(header + 12, gpt->header_size);
    put64(header + 72, gpt->table_lba);
    put32(header + 80, gpt->entries);
    put32(header + 84, gpt->entry_size);
    uint32_t table_crc = crc32_ieee(0, table, table_size);
    put32(header + 88, gpt->bad_table_crc ? table_crc ^ 1 : table_crc);
    uint32_t header_crc = crc32_ieee(0, header,
                                     gpt->header_size <= 512 ? gpt->header_size : 92);
    put32(header + 16, gpt->bad_header_crc ? header_crc ^ 1 : header_crc);
    int fd = open(path, O_WRONLY);
    CHECK(fd >= 0 && pwrite(fd, mbr, 512, 0) == 512 &&
          pwrite(fd, header, 512, 512) == 512);
    long long disk = gpt->size ? gpt->size : 8 * GIB;
    if (fd >= 0 && table_size && gpt->table_lba < (uint64_t)(disk / 512) &&
        (long long)(gpt->table_lba * 512 + table_size) <= disk)
        CHECK(pwrite(fd, table, table_size, (off_t)(gpt->table_lba * 512)) ==
              (ssize_t)table_size);
    if (fd >= 0)
        close(fd);
    free(table);
}

static void test_raw_gpt_checks(void)
{
    struct image_error error;
    reset_root();
    const struct gpt_fixture valid = { .table_lba = 2, .entries = 128,
                                       .entry_size = 128, .header_size = 92 };
    write_gpt(path_of("raw"), &valid);
    CHECK(raw_check(path_of("raw"), &error) == 0);
    /* The Python test's corruption: one partition-table byte changes. */
    int fd = open(path_of("raw"), O_WRONLY);
    CHECK(fd >= 0 && pwrite(fd, "\x02", 1, 1024) == 1 && close(fd) == 0);
    CHECK_FAILS(raw_check(path_of("raw"), &error), error, "partition table CRC");

    struct { struct gpt_fixture gpt; const char *reason; } cases[] = {
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 92,
            .size = 8 * GIB - 512 }, "must be 8 GiB" },
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 92,
            .no_boot_signature = 1 }, "protective MBR" },
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 92,
            .wrong_type = 1 }, "protective MBR" },
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 92,
            .bad_signature = 1 }, "invalid GPT header" },
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 91 },
          "invalid GPT header" },
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 513 },
          "invalid GPT header" },
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 92,
            .bad_header_crc = 1 }, "GPT header CRC" },
        { { .table_lba = 2, .entries = 0, .entry_size = 128, .header_size = 92 },
          "partition table bounds" },
        { { .table_lba = 2, .entries = 128, .entry_size = 64, .header_size = 92 },
          "partition table bounds" },
        { { .table_lba = 2, .entries = 131073, .entry_size = 128, .header_size = 92 },
          "partition table bounds" },
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 92,
            .bad_table_crc = 1 }, "partition table CRC" },
        { { .table_lba = 2, .entries = 128, .entry_size = 128, .header_size = 92,
            .empty_table = 1 }, "empty GPT partition table" },
        { { .table_lba = 16777215, .entries = 128, .entry_size = 128,
            .header_size = 92 }, "partition table CRC" },
        { { .table_lba = UINT64_MAX / 256, .entries = 128, .entry_size = 128,
            .header_size = 92 }, "partition table CRC" },
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        write_gpt(path_of("raw"), &cases[index].gpt);
        CHECK_FAILS(raw_check(path_of("raw"), &error), error,
                    cases[index].reason);
    }
    /* The maximum 16 MiB entry array and a 512-byte header are accepted. */
    const struct gpt_fixture largest = { .table_lba = 2, .entries = 131072,
                                         .entry_size = 128, .header_size = 512 };
    write_gpt(path_of("raw"), &largest);
    CHECK(raw_check(path_of("raw"), &error) == 0);
    CHECK_FAILS(raw_check(path_of("missing"), &error), error, "cannot inspect");
}

/* ---- variation inputs --------------------------------------------------- */

static void expect_plan(long long seed, long long count,
                        const uint32_t *seeds, const unsigned *bytes)
{
    struct variation_case cases[VARIATION_MAX_CASES];
    struct image_error error;
    CHECK(variations_plan(seed, count, cases, &error) == 0);
    for (long long index = 0; index < count; index++) {
        if (cases[index].index != (unsigned)index ||
            cases[index].seed != seeds[index] ||
            cases[index].regenerable_bytes != bytes[index]) {
            fprintf(stderr, "FAIL: plan(%lld) case %lld = %u/%u, expected %u/%u\n",
                    seed, index, cases[index].seed,
                    cases[index].regenerable_bytes, seeds[index], bytes[index]);
            failures++;
        }
    }
}

static void expect_file_sha(const char *path, const char *expected)
{
    char hex[SHA256_HEX_CAP];
    struct image_error error;
    if (sha256_file(path, hex, &error) != 0 || strcmp(hex, expected) != 0) {
        fprintf(stderr, "FAIL: %s has digest %s, expected %s\n", path,
                hex, expected);
        failures++;
    }
}

static void test_variation_inputs(void)
{
    /* CPython 3: image_build_variations.variations(seed, count). */
    const uint32_t seeds_a[] = { 3739848299u, 157876371u, 3554152109u, 3082041475u };
    const unsigned bytes_a[] = { 0, 6121, 57895, 25868 };
    expect_plan(20260921, 4, seeds_a, bytes_a);
    expect_plan(20260921, 2, seeds_a, bytes_a);
    const uint32_t seeds_b[] = { 1551038599u, 2230687110u };
    const unsigned bytes_b[] = { 0, 5303 };
    expect_plan(20260922, 2, seeds_b, bytes_b);
    const uint32_t seeds_c[] = { 3626764237u };
    const unsigned bytes_c[] = { 0 };
    expect_plan(0, 1, seeds_c, bytes_c);
    const uint32_t seeds_d[] = { 872737089u, 1201118819u, 2135565362u };
    const unsigned bytes_d[] = { 0, 59838, 5236 };
    expect_plan(4294967295LL, 3, seeds_d, bytes_d);

    struct variation_case cases[VARIATION_MAX_CASES];
    struct image_error error;
    const long long bad[][2] = { { -1, 2 }, { 4294967296LL, 2 }, { 0, 0 }, { 0, 5 } };
    for (size_t index = 0; index < sizeof(bad) / sizeof(bad[0]); index++)
        CHECK_FAILS(variations_plan(bad[index][0], bad[index][1], cases, &error),
                    error, "seed must be uint32");

    reset_root();
    CHECK(mkdir(path_of("case1"), 0700) == 0 && mkdir(path_of("case0"), 0700) == 0);
    struct variation_case one = { 1, 157876371u, 6121 };
    CHECK(variations_write_payload(&one, path_of("case1"), &error) == 0);
    expect_file_sha(path_of("case1/log"),
        "1433ece8e893e9254a5623412f2b3a03934fec71f65e56c44984e2667febc8c8");
    expect_file_sha(path_of("case1/deb"),
        "47c7e7de03c4bb307a407618123bf56437c870f401d17b1704cb75dc1edee9c2");
    expect_file_sha(path_of("case1/tmp"),
        "895933f231e060fd9d12edc3ced096d13e40cccf303b8ed12a1d12be1bd73f7d");
    expect_file_sha(path_of("case1/var-tmp"),
        "6748d15f50d1cbad5d7b026291e0a051f1c6b68df9d62b4096d9e8397aa08013");
    expect_file_sha(path_of("case1/ssh-host-key"),
        "e1aac5b41ed6f78a7f87e48512175c63f5ceca9e3016da63a1ebea501c8dcfff");
    char *machine_id = read_all(path_of("case1/machine-id"), NULL);
    CHECK(machine_id && strcmp(machine_id, "12d050867ba579e859bd847157712e59\n") == 0);
    free(machine_id);
    /* Generated bytes never replace an existing payload. */
    CHECK_FAILS(variations_write_payload(&one, path_of("case1"), &error), error,
                "cannot create");

    struct variation_case zero = { 0, 3739848299u, 0 };
    CHECK(variations_write_payload(&zero, path_of("case0"), &error) == 0);
    size_t length = 1;
    char *log = read_all(path_of("case0/log"), &length);
    CHECK(log && length == 0);
    free(log);
    machine_id = read_all(path_of("case0/machine-id"), NULL);
    CHECK(machine_id && strcmp(machine_id, "1e1c48bbe7c9c8f2597954596cff0f07\n") == 0);
    free(machine_id);
    expect_file_sha(path_of("case0/ssh-host-key"),
        "054eb34bc878b4ef22162c1d18c671fa63cda08a7d2ca2fa1900c013f7397aa3");

    /* The guest command is fixed text naming only guest-owned targets. */
    const char *targets[] = {
        "/var/log/hamn-generated.log", "/var/cache/apt/archives/hamn-generated.deb",
        "/tmp/hamn-generated", "/var/tmp/hamn-generated", "/etc/machine-id",
        "/etc/ssh/ssh_host_generated_key", "rm -rf \"$s\"", "set -eu",
    };
    for (size_t index = 0; index < sizeof(targets) / sizeof(targets[0]); index++)
        CHECK(strstr(variations_guest_install_command, targets[index]) != NULL);
    CHECK(strstr(variations_guest_install_command, "python") == NULL);

    struct variations_options options = {
        .source_root = root, .baseline = path_of("b"),
        .size_report = path_of("r"), .output_directory = path_of("o"),
        .seed = 1, .case_count = 5,
    };
    CHECK_FAILS(variations_run(&options, &error), error, "seed must be uint32");
#ifndef __linux__
    options.case_count = 1;
    CHECK_FAILS(variations_run(&options, &error), error,
                "real image variations require Linux arm64");
    CHECK(!exists(path_of("o")));
#endif
}

/* ---- variation orchestration with fake builder tools (HAMN_TEST) -------- */

static void write_script(const char *path, const char *body)
{
    write_text(path, body);
    CHECK(chmod(path, 0755) == 0);
}

#define FAKE_REVISION "0123456789abcdef0123456789abcdef01234567"

static void fake_builder_tools(void)
{
    CHECK(mkdir(path_of("bin"), 0755) == 0);
    /* Every call is recorded; conversions copy files or the raw fixture. */
    write_script(path_of("bin/qemu-img"),
        "#!/bin/bash\nset -euo pipefail\n"
        "printf '%s\\n' \"$*\" >>\"$HAMN_FAKE_CALLS\"\n"
        "args=(\"$@\")\nsrc=${args[$(($# - 2))]}\ndst=${args[$(($# - 1))]}\n"
        "if [ -n \"${HAMN_FAKE_HANG:-}\" ]; then\n"
        "    sleep 300 & echo $! >\"$HAMN_FAKE_HANG\"; wait\nfi\n"
        "case \"$1 $*\" in\n"
        "compare*) exit 0 ;;\n"
        "*' -O raw '*) cp \"$HAMN_FAKE_RAW\" \"$dst\" ;;\n"
        "*' -c '*) head -c 4096 \"$src\" >\"$dst\" ;;\n"
        "convert*) cp \"$src\" \"$dst\" ;;\n"
        "*) exit 2 ;;\nesac\n");
    /* The payload directory is captured for byte comparisons. */
    write_script(path_of("bin/virt-customize"),
        "#!/bin/bash\nset -euo pipefail\n"
        "printf '%s\\n' \"$@\" >>\"$HAMN_FAKE_CUSTOMIZE\"\n"
        "while [ $# -gt 0 ]; do\n"
        "    if [ \"$1\" = --copy-in ]; then\n"
        "        n=$(ls \"$HAMN_FAKE_PAYLOADS\" | wc -l | tr -d ' ')\n"
        "        cp -R \"${2%:/root}\" \"$HAMN_FAKE_PAYLOADS/$n\"\n"
        "    fi\n    shift\ndone\n");
    write_script(path_of("bin/guestfish"),
        "#!/bin/bash\nset -euo pipefail\n"
        "for argument; do\n"
        "    if [ \"$argument\" = command ]; then printf '%b' \"$HAMN_FAKE_INVENTORY\"; fi\n"
        "done\n");
    /* The fake decoder "extracts" the valid raw GPT fixture. */
    write_script(path_of("bin/cc"),
        "#!/bin/bash\nset -euo pipefail\n"
        "while [ $# -gt 1 ] && [ \"$1\" != -o ]; do shift; done\n"
        "printf '#!/bin/sh\\ncp \"$HAMN_FAKE_RAW\" \"$2\"\\n' >\"$2\"\n"
        "chmod 0755 \"$2\"\n");
    write_script(path_of("bin/git"),
        "#!/bin/sh\n[ \"$3\" = rev-parse ] && echo " FAKE_REVISION "\n");
}

static void test_variation_orchestration(void)
{
    struct image_error error;
    reset_root();
    fake_builder_tools();
    CHECK(mkdir(path_of("src"), 0755) == 0 && mkdir(path_of("export"), 0755) == 0 &&
          mkdir(path_of("payloads"), 0755) == 0);
    /* A 1 MiB raw GPT (test builds only) keeps the per-case hashing small. */
    const struct gpt_fixture valid = { .table_lba = 2, .entries = 128,
                                       .entry_size = 128, .header_size = 92,
                                       .size = MIB };
    write_gpt(path_of("fixture.raw"), &valid);
    setenv("HAMN_TEST_RAW_VIRTUAL_BYTES", "1048576", 1);
    char raw_sha[SHA256_HEX_CAP];
    CHECK(sha256_file(path_of("fixture.raw"), raw_sha, &error) == 0);
    /* A genuine same-build report for the exported baseline. */
    sparse(path_of("export/baseline.img"), BASELINE_BYTES);
    sparse(path_of("candidate"), CANDIDATE_BYTES);
    write_text(path_of("packages.tsv"), "docker.io\t28.0\t12345\nrunc\t1.3.4\t34734\n");
    struct size_gate_options report = gate_options(1);
    report.baseline = path_of("export/baseline.img");
    report.report = path_of("export/size-report.json");
    CHECK(size_gate_run(&report, &error) == 0);

    char path[8192];
    snprintf(path, sizeof(path), "%s:/usr/bin:/bin", path_of("bin"));
    setenv("PATH", path, 1);
    setenv("HAMN_TEST_VARIATIONS_HOST", "1", 1);
    setenv("HAMN_FAKE_CALLS", path_of("qemu-calls"), 1);
    setenv("HAMN_FAKE_CUSTOMIZE", path_of("customize-args"), 1);
    setenv("HAMN_FAKE_PAYLOADS", path_of("payloads"), 1);
    setenv("HAMN_FAKE_RAW", path_of("fixture.raw"), 1);
    setenv("HAMN_FAKE_INVENTORY", "docker.io\\t28.0\\t12345\\nrunc\\t1.3.4\\t34734\\n\\n", 1);
    unsetenv("HAMN_FAKE_HANG");
    unsetenv("HAMN_TEST_VARIATIONS_TIMEOUT");
    /* path_of() reuses its buffers; options need their own copies. */
    char source_root[4096], baseline[4096], size_report[4096], output[4096];
    snprintf(source_root, sizeof(source_root), "%s", path_of("src"));
    snprintf(baseline, sizeof(baseline), "%s", path_of("export/baseline.img"));
    snprintf(size_report, sizeof(size_report), "%s",
             path_of("export/size-report.json"));
    snprintf(output, sizeof(output), "%s", path_of("out"));
    struct variations_options options = {
        .source_root = source_root, .baseline = baseline,
        .size_report = size_report, .output_directory = output,
        .seed = 20260921, .case_count = 2,
    };
    CHECK(variations_run(&options, &error) == 0);
    if (failures)
        fprintf(stderr, "variations: %s\n", error.message);

    struct json_value *index = load_json(path_of("out/variations.json"));
    const struct json_value *variants = index ? json_object_get(index, "variants") : NULL;
    long long number = 0;
    CHECK(index && json_integer_value(json_object_get(index, "seed"), &number) == 0 &&
          number == 20260921);
    CHECK(index && json_string_equals(json_object_get(index, "baselineSha256"),
                                      ZERO_BASELINE_SHA));
    CHECK(index && json_string_equals(json_object_get(index, "sourceRevision"),
                                      FAKE_REVISION));
    CHECK(index && json_string_equals(json_object_get(index,
                                      "physicalRuntimeValidation"), "pending"));
    CHECK(variants && variants->count == 2);
    if (variants && variants->count == 2) {
        const struct json_value *second = variants->members[1].value;
        char image_sha[SHA256_HEX_CAP];
        CHECK(sha256_file(path_of("out/case-1.img"), image_sha, &error) == 0);
        CHECK(json_integer_value(json_object_get(second, "seed"), &number) == 0 &&
              number == 157876371);
        CHECK(json_integer_value(json_object_get(second, "regenerableBytesPerFile"),
                                 &number) == 0 && number == 6121);
        CHECK(json_string_equals(json_object_get(second, "image"), "case-1.img"));
        CHECK(json_string_equals(json_object_get(second, "imageSha256"), image_sha));
        CHECK(json_string_equals(json_object_get(second, "rawSha256"), raw_sha));
        CHECK(json_string_equals(json_object_get(second, "sizeReport"),
                                 "case-1.size-report.json"));
        CHECK(json_string_equals(json_object_get(second, "structuralChecks"), "passed"));
    }
    json_free(index);
    struct json_value *case_report = load_json(path_of("out/case-1.size-report.json"));
    CHECK(case_report && json_object_get(case_report, "reviewOnly")->type == JSON_TRUE);
    json_free(case_report);
    CHECK(exists(path_of("out/case-0.size-report.budget-proposal.json")));
    CHECK(exists(path_of("out/decoder-build.log")) && exists(path_of("out/case-1.log")));
    char *log = read_all(path_of("out/case-0.log"), NULL);
    CHECK(log && strstr(log, "[\"qemu-img\", \"convert\", \"-q\"") &&
          strstr(log, "[\"hamn-image-tool\", \"verify-raw\"") &&
          strstr(log, "docker.io\t28.0\t12345\nrunc"));
    /* As in the builder, the journal is recreated and checked before fstrim. */
    CHECK(log && strstr(log,
        "\"run\", \":\", \"debug\", \"sh\", "
        "\"tune2fs -O ^has_journal /dev/sda3 && tune2fs -j /dev/sda3\", \":\", "
        "\"e2fsck-f\", \"/dev/sda3\", \":\", \"mount\", \"/dev/sda3\", \"/\", "
        "\":\", \"fstrim\", \"/\""));
    free(log);
    /* The copied-in payload holds the CPython-compatible generated bytes. */
    expect_file_sha(path_of("payloads/1/log"),
        "1433ece8e893e9254a5623412f2b3a03934fec71f65e56c44984e2667febc8c8");
    char *customize = read_all(path_of("customize-args"), NULL);
    CHECK(customize && strstr(customize, variations_guest_install_command) &&
          strstr(customize, "bash /root/hamn-slim.sh && rm /root/hamn-slim.sh"));
    free(customize);
    DIR *listing = opendir(path_of("out"));
    struct dirent *entry;
    while (listing && (entry = readdir(listing)))
        CHECK(strncmp(entry->d_name, ".variation-stage-", 17) != 0);
    if (listing)
        closedir(listing);

    /* An existing output directory is never reused. */
    CHECK_FAILS(variations_run(&options, &error), error,
                "cannot create variation output");
    /* A cleanup that changes the pinned package set fails the case. */
    snprintf(output, sizeof(output), "%s", path_of("changed"));
    setenv("HAMN_FAKE_INVENTORY", "docker.io\\t29.0\\t12345\\nrunc\\t1.3.4\\t34734\\n", 1);
    CHECK_FAILS(variations_run(&options, &error), error, "changed the pinned");
    CHECK(exists(path_of("changed/case-0.log")) && !exists(path_of("changed/case-0.img")));
    /* A hung tool is terminated with its whole process group. */
    snprintf(output, sizeof(output), "%s", path_of("hung"));
    setenv("HAMN_FAKE_HANG", path_of("hung-child.pid"), 1);
    setenv("HAMN_TEST_VARIATIONS_TIMEOUT", "1", 1);
    CHECK_FAILS(variations_run(&options, &error), error, "timed out");
    char *pid_text = read_all(path_of("hung-child.pid"), NULL);
    pid_t child = pid_text ? (pid_t)atoi(pid_text) : 0;
    free(pid_text);
    int gone = 0;
    for (int attempt = 0; child > 0 && attempt < 100 && !gone; attempt++) {
        gone = kill(child, 0) != 0 && errno == ESRCH;
        if (!gone)
            usleep(50000);
    }
    CHECK(child > 0 && gone);
    unsetenv("HAMN_FAKE_HANG");
    unsetenv("HAMN_TEST_VARIATIONS_TIMEOUT");
    unsetenv("HAMN_TEST_VARIATIONS_HOST");
    unsetenv("HAMN_TEST_RAW_VIRTUAL_BYTES");
    /* The input baseline is never modified. */
    char after[SHA256_HEX_CAP];
    CHECK(sha256_file(path_of("export/baseline.img"), after, &error) == 0 &&
          strcmp(after, ZERO_BASELINE_SHA) == 0);
}

int main(void)
{
    /* Fixture files must not be group/other-writable under any caller umask. */
    umask(022);
    test_digest_vectors();
    test_evidence_publication();
    test_evidence_paths();
    test_evidence_rejects_mismatched_sources();
    test_evidence_faults_roll_back();
    test_evidence_race_preserves_competitor();
    test_size_report_layout_and_budget_flow();
    test_size_limits_cannot_be_bypassed();
    test_size_budget_rejections();
    test_budget_schema();
    test_release_boundary();
    test_inventory_newlines_and_pinning();
    test_invalid_inventories_never_publish_reports();
    test_raw_gpt_checks();
    test_variation_inputs();
    test_variation_orchestration();
    if (root[0])
        nftw(root, remove_entry, 16, FTW_DEPTH | FTW_PHYS);
    if (failures) {
        fprintf(stderr, "FAIL: %d image tool checks failed\n", failures);
        return 1;
    }
    printf("PASS: image evidence, size gates, GPT checks and variation inputs\n");
    return 0;
}
