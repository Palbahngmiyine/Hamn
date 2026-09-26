#include "image/variations.h"

#include <errno.h>
#include <fcntl.h>
#include <ftw.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "image/digest.h"
#include "image/raw_check.h"
#include "image/size_gate.h"

#define MT_STATE_WORDS 624
#define COMMAND_TIMEOUT_SECONDS 1200
#define INVENTORY_TIMEOUT_SECONDS 300
#define GIT_TIMEOUT_SECONDS 5
#define TERMINATION_GRACE_MS 5000
#define POLL_INTERVAL_MS 50
#define PAYLOAD_DIRECTORY "hamn-generated"
/* Must match build-ubuntu-24.04-arm64.sh; guest/tests checks both. */
#define JOURNAL_RESET "tune2fs -O ^has_journal /dev/sda3 && tune2fs -j /dev/sda3"

/*
 * Fixed guest-side installer: generated input is data copied in beside it,
 * never a shell fragment. cat > preserves an existing file's owner and mode
 * like the Python write_bytes()/write_text() it replaces.
 */
const char variations_guest_install_command[] =
    "set -eu; s=/root/" PAYLOAD_DIRECTORY "; "
    "mkdir -p /var/log /var/cache/apt/archives /tmp /var/tmp; "
    "cat \"$s/log\" >/var/log/hamn-generated.log; "
    "cat \"$s/deb\" >/var/cache/apt/archives/hamn-generated.deb; "
    "cat \"$s/tmp\" >/tmp/hamn-generated; "
    "cat \"$s/var-tmp\" >/var/tmp/hamn-generated; "
    "cat \"$s/machine-id\" >/etc/machine-id; "
    "cat \"$s/ssh-host-key\" >/etc/ssh/ssh_host_generated_key; "
    "rm -rf \"$s\"";

/* CPython's Mersenne Twister (Modules/_randommodule.c). */
struct mt19937 {
    uint32_t state[MT_STATE_WORDS];
    unsigned index;
};

static void mt_init_genrand(struct mt19937 *mt, uint32_t seed)
{
    mt->state[0] = seed;
    for (unsigned index = 1; index < MT_STATE_WORDS; index++) {
        uint32_t previous = mt->state[index - 1];
        mt->state[index] = 1812433253u * (previous ^ (previous >> 30)) + index;
    }
    mt->index = MT_STATE_WORDS;
}

/* random.Random(seed) for 0 <= seed < 2**32: init_by_array([seed]). */
static void mt_seed(struct mt19937 *mt, uint32_t seed)
{
    const uint32_t key[1] = { seed };
    const unsigned key_length = 1;
    mt_init_genrand(mt, 19650218u);
    unsigned i = 1, j = 0;
    for (unsigned k = MT_STATE_WORDS; k; k--) {
        uint32_t previous = mt->state[i - 1];
        mt->state[i] = (mt->state[i] ^ ((previous ^ (previous >> 30)) *
                                        1664525u)) + key[j] + j;
        i++;
        j++;
        if (i >= MT_STATE_WORDS) {
            mt->state[0] = mt->state[MT_STATE_WORDS - 1];
            i = 1;
        }
        if (j >= key_length)
            j = 0;
    }
    for (unsigned k = MT_STATE_WORDS - 1; k; k--) {
        uint32_t previous = mt->state[i - 1];
        mt->state[i] = (mt->state[i] ^ ((previous ^ (previous >> 30)) *
                                        1566083941u)) - i;
        i++;
        if (i >= MT_STATE_WORDS) {
            mt->state[0] = mt->state[MT_STATE_WORDS - 1];
            i = 1;
        }
    }
    mt->state[0] = 0x80000000u;
    mt->index = MT_STATE_WORDS;
}

static uint32_t mt_next(struct mt19937 *mt)
{
    static const uint32_t magnitude[2] = { 0x0u, 0x9908b0dfu };
    if (mt->index >= MT_STATE_WORDS) {
        unsigned k;
        uint32_t y;
        for (k = 0; k < MT_STATE_WORDS - 397; k++) {
            y = (mt->state[k] & 0x80000000u) | (mt->state[k + 1] & 0x7fffffffu);
            mt->state[k] = mt->state[k + 397] ^ (y >> 1) ^ magnitude[y & 1];
        }
        for (; k < MT_STATE_WORDS - 1; k++) {
            y = (mt->state[k] & 0x80000000u) | (mt->state[k + 1] & 0x7fffffffu);
            mt->state[k] = mt->state[k + (397 - MT_STATE_WORDS)] ^ (y >> 1) ^
                           magnitude[y & 1];
        }
        y = (mt->state[MT_STATE_WORDS - 1] & 0x80000000u) |
            (mt->state[0] & 0x7fffffffu);
        mt->state[MT_STATE_WORDS - 1] =
            mt->state[396] ^ (y >> 1) ^ magnitude[y & 1];
        mt->index = 0;
    }
    uint32_t y = mt->state[mt->index++];
    y ^= y >> 11;
    y ^= (y << 7) & 0x9d2c5680u;
    y ^= (y << 15) & 0xefc60000u;
    y ^= y >> 18;
    return y;
}

/* Random.getrandbits(bits) for 1 <= bits <= 64. */
static uint64_t mt_bits(struct mt19937 *mt, unsigned bits)
{
    if (bits <= 32)
        return mt_next(mt) >> (32 - bits);
    uint64_t low = mt_next(mt);
    uint64_t high = mt_next(mt) >> (64 - bits);
    return low | high << 32;
}

/* Random._randbelow_with_getrandbits(limit) for 1 <= limit <= 2**32. */
static uint64_t mt_below(struct mt19937 *mt, uint64_t limit)
{
    unsigned bits = 0;
    for (uint64_t value = limit; value; value >>= 1)
        bits++;
    uint64_t result = mt_bits(mt, bits);
    while (result >= limit)
        result = mt_bits(mt, bits);
    return result;
}

/*
 * Random.randbytes(length): getrandbits(8 * length) as little-endian bytes.
 * Each 32-bit word is little-endian; the last partial word keeps its top bits.
 */
static void mt_bytes(struct mt19937 *mt, unsigned char *out, size_t length)
{
    size_t offset = 0;
    while (offset < length) {
        size_t remaining = length - offset;
        uint32_t word = mt_next(mt);
        if (remaining < 4)
            word >>= 32 - 8 * remaining;
        for (size_t index = 0; index < 4 && offset < length; index++)
            out[offset++] = (unsigned char)(word >> (8 * index));
    }
}

int variations_plan(long long seed, long long count,
                    struct variation_case cases[VARIATION_MAX_CASES],
                    struct image_error *error)
{
    if (seed < 0 || seed > 0xFFFFFFFFLL || count < 1 ||
        count > VARIATION_MAX_CASES)
        return image_fail(error,
            "seed must be uint32 and case count must be between one and four");
    struct mt19937 mt;
    mt_seed(&mt, (uint32_t)seed);
    for (unsigned index = 0; index < (unsigned)count; index++) {
        cases[index].index = index;
        cases[index].seed = (uint32_t)mt_below(&mt, 1ULL << 32);
        cases[index].regenerable_bytes =
            index == 0 ? 0 : (unsigned)(1024 + mt_below(&mt, 65537 - 1024));
    }
    return 0;
}

static int write_new_file(const char *directory, const char *name,
                          const void *bytes, size_t length,
                          struct image_error *error)
{
    size_t path_length = strlen(directory) + strlen(name) + 2;
    char *path = malloc(path_length);
    if (!path)
        return image_fail(error, "out of memory");
    snprintf(path, path_length, "%s/%s", directory, name);
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
                  0600);
    if (fd < 0) {
        image_fail_errno(error, "cannot create", path);
        free(path);
        return -1;
    }
    const unsigned char *cursor = bytes;
    size_t left = length;
    int rc = 0;
    while (left > 0) {
        ssize_t written = write(fd, cursor, left);
        if (written < 0 && errno == EINTR)
            continue;
        if (written < 0) {
            rc = image_fail_errno(error, "cannot write", path);
            break;
        }
        cursor += written;
        left -= (size_t)written;
    }
    if (close(fd) != 0 && rc == 0)
        rc = image_fail_errno(error, "cannot write", path);
    free(path);
    return rc;
}

int variations_write_payload(const struct variation_case *variation,
                             const char *directory, struct image_error *error)
{
    static const char *const generated[] = { "log", "deb", "tmp", "var-tmp" };
    struct mt19937 mt;
    mt_seed(&mt, variation->seed);
    unsigned char *bytes = malloc(variation->regenerable_bytes ?
                                  variation->regenerable_bytes : 1);
    if (!bytes)
        return image_fail(error, "out of memory");
    for (size_t index = 0; index < sizeof(generated) / sizeof(generated[0]);
         index++) {
        mt_bytes(&mt, bytes, variation->regenerable_bytes);
        if (write_new_file(directory, generated[index], bytes,
                           variation->regenerable_bytes, error) != 0) {
            free(bytes);
            return -1;
        }
    }
    free(bytes);
    unsigned char identity[16], key[64];
    char machine_id[sizeof(identity) * 2 + 2];
    mt_bytes(&mt, identity, sizeof(identity));
    for (size_t index = 0; index < sizeof(identity); index++)
        snprintf(machine_id + index * 2, 3, "%02x", identity[index]);
    machine_id[sizeof(identity) * 2] = '\n';
    machine_id[sizeof(identity) * 2 + 1] = '\0';
    mt_bytes(&mt, key, sizeof(key));
    if (write_new_file(directory, "machine-id", machine_id,
                       sizeof(identity) * 2 + 1, error) != 0 ||
        write_new_file(directory, "ssh-host-key", key, sizeof(key), error) != 0)
        return -1;
    return 0;
}

/* Command execution with process-group ownership and bounded deadlines. */

static volatile sig_atomic_t interrupted_signal;

static void record_signal(int signal_number)
{
    interrupted_signal = signal_number;
}

static const int guarded_signals[] = { SIGINT, SIGTERM, SIGHUP };
#define GUARDED_SIGNAL_COUNT (sizeof(guarded_signals) / sizeof(guarded_signals[0]))

static long long monotonic_ms(void)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (long long)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static void sleep_ms(long milliseconds)
{
    struct timespec pause = { milliseconds / 1000,
                              (milliseconds % 1000) * 1000000L };
    nanosleep(&pause, NULL);
}

/* Waits up to GRACE_MS for PID; returns 1 once reaped. */
static int reap_within(pid_t pid, int *status, long long grace_ms)
{
    long long deadline = monotonic_ms() + grace_ms;
    for (;;) {
        pid_t done = waitpid(pid, status, WNOHANG);
        if (done == pid || (done < 0 && errno != EINTR))
            return 1;
        if (monotonic_ms() >= deadline)
            return 0;
        sleep_ms(POLL_INTERVAL_MS);
    }
}

/* The unreaped PID still names our session leader, so its group is ours. */
static void terminate_group(pid_t pid, int *status)
{
    kill(-pid, SIGTERM);
    if (reap_within(pid, status, TERMINATION_GRACE_MS))
        return;
    kill(-pid, SIGKILL);
    reap_within(pid, status, TERMINATION_GRACE_MS);
}

static int log_command(const char *log_path, char *const argv[],
                       struct image_error *error)
{
    struct json_buffer line = { 0 };
    int ok = json_buffer_append(&line, "[", 1) == 0;
    for (size_t index = 0; ok && argv[index]; index++) {
        ok = (index == 0 || json_buffer_append(&line, ", ", 2) == 0) &&
             json_buffer_append(&line, "\"", 1) == 0 &&
             json_escape_ascii(argv[index], strlen(argv[index]), &line) == 0 &&
             json_buffer_append(&line, "\"", 1) == 0;
    }
    ok = ok && json_buffer_append(&line, "]\n", 2) == 0;
    int fd = ok ? open(log_path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC,
                       0666) : -1;
    int rc = 0;
    if (!ok)
        rc = image_fail(error, "cannot format command log for %s", argv[0]);
    else if (fd < 0 || write(fd, line.data, line.length) != (ssize_t)line.length)
        rc = image_fail_errno(error, "cannot write command log", log_path);
    if (fd >= 0)
        close(fd);
    json_buffer_free(&line);
    return rc;
}

static int append_log(const char *log_path, const struct json_buffer *data,
                      struct image_error *error)
{
    int fd = open(log_path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0666);
    int rc = 0;
    if (fd < 0 || (data->length &&
                   write(fd, data->data, data->length) != (ssize_t)data->length))
        rc = image_fail_errno(error, "cannot write command log", log_path);
    if (fd >= 0)
        close(fd);
    return rc;
}

/*
 * Runs ARGV in a new session with stdin from /dev/null. With LOG_PATH, the
 * command line (JSON) is appended first and stdout/stderr go to the log;
 * without it, stderr is inherited. CAPTURED receives stdout instead (and is
 * appended to the log after success). The whole process group is terminated
 * on timeout or a received SIGINT/SIGTERM/SIGHUP.
 */
static int run_command(char *const argv[], const char *log_path,
                       unsigned timeout_seconds, struct json_buffer *captured,
                       struct image_error *error)
{
#ifdef HAMN_TEST
    /* Test builds only: exercise deadlines without waiting minutes. */
    const char *test_timeout = getenv("HAMN_TEST_VARIATIONS_TIMEOUT");
    if (test_timeout && *test_timeout)
        timeout_seconds = (unsigned)strtoul(test_timeout, NULL, 10);
#endif
    if (log_path && log_command(log_path, argv, error) != 0)
        return -1;
    int log_fd = -1, null_fd = -1, pipe_fds[2] = { -1, -1 };
    if (log_path) {
        log_fd = open(log_path, O_WRONLY | O_APPEND | O_CLOEXEC);
        if (log_fd < 0)
            return image_fail_errno(error, "cannot open command log", log_path);
    }
    null_fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    if (null_fd < 0 || (captured && pipe(pipe_fds) != 0)) {
        image_fail_errno(error, "cannot prepare command", argv[0]);
        goto fail_before_fork;
    }
    if (captured && (fcntl(pipe_fds[0], F_SETFD, FD_CLOEXEC) != 0 ||
                     fcntl(pipe_fds[1], F_SETFD, FD_CLOEXEC) != 0)) {
        image_fail_errno(error, "cannot prepare command", argv[0]);
        goto fail_before_fork;
    }
    pid_t pid = fork();
    if (pid < 0) {
        image_fail_errno(error, "cannot start", argv[0]);
        goto fail_before_fork;
    }
    if (pid == 0) {
        setsid();
        dup2(null_fd, STDIN_FILENO);
        if (captured)
            dup2(pipe_fds[1], STDOUT_FILENO);
        else if (log_fd >= 0)
            dup2(log_fd, STDOUT_FILENO);
        if (log_fd >= 0)
            dup2(log_fd, STDERR_FILENO);
        execvp(argv[0], argv);
        _exit(127);
    }
    close(null_fd);
    if (captured) {
        close(pipe_fds[1]);
        pipe_fds[1] = -1;
    }
    long long deadline = monotonic_ms() + (long long)timeout_seconds * 1000;
    int status = 0, reaped = 0, reading = captured != NULL;
    const char *failure = NULL;
    while (!failure && (!reaped || reading)) {
        if (reading) {
            struct pollfd ready = { pipe_fds[0], POLLIN, 0 };
            int count = poll(&ready, 1, POLL_INTERVAL_MS);
            if (count > 0) {
                char chunk[65536];
                ssize_t bytes = read(pipe_fds[0], chunk, sizeof(chunk));
                if (bytes > 0) {
                    if (json_buffer_append(captured, chunk, (size_t)bytes) != 0)
                        failure = "ran out of memory capturing output";
                } else if (bytes == 0) {
                    reading = 0;
                } else if (errno != EINTR && errno != EAGAIN) {
                    failure = "could not read its output";
                }
            } else if (count < 0 && errno != EINTR) {
                failure = "could not read its output";
            }
        } else {
            sleep_ms(POLL_INTERVAL_MS);
        }
        if (!reaped) {
            pid_t done = waitpid(pid, &status, WNOHANG);
            if (done == pid)
                reaped = 1;
            else if (done < 0 && errno != EINTR)
                failure = "could not be waited for";
        }
        if (!failure && interrupted_signal)
            failure = "was interrupted by a signal";
        else if (!failure && monotonic_ms() > deadline)
            failure = "timed out";
    }
    /* Python's Popen handling: only an unreaped leader's group is killed. */
    if (failure && !reaped)
        terminate_group(pid, &status);
    if (pipe_fds[0] >= 0)
        close(pipe_fds[0]);
    if (log_fd >= 0)
        close(log_fd);
    if (failure)
        return image_fail(error, "%s %s after at most %u seconds%s%s",
                          argv[0], failure, timeout_seconds,
                          log_path ? "; see " : "", log_path ? log_path : "");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        return image_fail(error, "%s failed with %s %d%s%s", argv[0],
                          WIFEXITED(status) ? "exit status" : "signal",
                          WIFEXITED(status) ? WEXITSTATUS(status) :
                                              WTERMSIG(status),
                          log_path ? "; see " : "", log_path ? log_path : "");
    }
    if (captured && log_path)
        return append_log(log_path, captured, error);
    return 0;

fail_before_fork:
    if (null_fd >= 0)
        close(null_fd);
    if (pipe_fds[0] >= 0)
        close(pipe_fds[0]);
    if (pipe_fds[1] >= 0)
        close(pipe_fds[1]);
    if (log_fd >= 0)
        close(log_fd);
    return -1;
}

static int executable_in_path(const char *name)
{
    const char *path = getenv("PATH");
    if (!path || !*path)
        path = "/usr/bin:/bin";
    size_t name_length = strlen(name);
    while (*path) {
        const char *end = strchr(path, ':');
        size_t length = end ? (size_t)(end - path) : strlen(path);
        char candidate[4096];
        if (length + name_length + 2 < sizeof(candidate)) {
            if (length == 0)
                snprintf(candidate, sizeof(candidate), "./%s", name);
            else
                snprintf(candidate, sizeof(candidate), "%.*s/%s", (int)length,
                         path, name);
            struct stat info;
            if (stat(candidate, &info) == 0 && S_ISREG(info.st_mode) &&
                access(candidate, X_OK) == 0)
                return 1;
        }
        if (!end)
            break;
        path = end + 1;
    }
    return 0;
}

static int remove_entry(const char *path, const struct stat *info, int type,
                        struct FTW *walk)
{
    (void)info;
    (void)walk;
    return type == FTW_DP ? rmdir(path) : unlink(path);
}

static int remove_tree(const char *path)
{
    return nftw(path, remove_entry, 16, FTW_DEPTH | FTW_PHYS);
}

static char *path_join(const char *directory, const char *name)
{
    size_t length = strlen(directory) + strlen(name) + 2;
    char *path = malloc(length);
    if (path)
        snprintf(path, length, "%s/%s", directory, name);
    return path;
}

static int string_arrays_equal(const struct json_value *left,
                               const struct json_value *right)
{
    if (!left || !right || left->type != JSON_ARRAY ||
        right->type != JSON_ARRAY || left->count != right->count)
        return 0;
    for (size_t index = 0; index < left->count; index++) {
        const struct json_value *a = left->members[index].value;
        const struct json_value *b = right->members[index].value;
        if (a->type != JSON_STRING || b->type != JSON_STRING ||
            a->length != b->length || memcmp(a->text, b->text, a->length) != 0)
            return 0;
    }
    return 1;
}

int variations_verify_inventory(const char *path,
                                const struct json_value *expected,
                                struct image_error *error)
{
    struct json_value *inventory = NULL;
    if (size_package_inventory(path, &inventory, error) != 0)
        return -1;
    int equal = string_arrays_equal(inventory, expected);
    json_free(inventory);
    if (!equal)
        return image_fail(error,
            "generated cleanup changed the pinned post-cleanup package inventory");
    return 0;
}

static int nonempty_string_array(const struct json_value *value)
{
    if (!value || value->type != JSON_ARRAY || value->count == 0)
        return 0;
    for (size_t index = 0; index < value->count; index++) {
        if (value->members[index].value->type != JSON_STRING)
            return 0;
    }
    return 1;
}

struct run_state {
    const struct variations_options *options;
    struct json_value *report;
    struct json_value *evidence;
    struct json_value *variants;
    char *work;
    char *decoder;
    char *packages_before;
    char *budget;
    const char *base_sha256;
    const char *source_revision;
    char baseline_sha[SHA256_HEX_CAP];  /* verified against the report */
};

static int add(struct json_value *object, const char *key,
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

static int write_packages_before(struct run_state *state,
                                 struct image_error *error)
{
    const struct json_value *rows = json_object_get(state->report,
                                                    "packagesBefore");
    struct json_buffer text = { 0 };
    for (size_t index = 0; index < rows->count; index++) {
        const struct json_value *row = rows->members[index].value;
        if ((index > 0 && json_buffer_append(&text, "\n", 1) != 0) ||
            json_buffer_append(&text, row->text, row->length) != 0) {
            json_buffer_free(&text);
            return image_fail(error, "out of memory");
        }
    }
    if (json_buffer_append(&text, "\n", 1) != 0) {
        json_buffer_free(&text);
        return image_fail(error, "out of memory");
    }
    int rc = image_write_file(state->packages_before, text.data, text.length,
                              error);
    json_buffer_free(&text);
    return rc;
}

static int run_case(struct run_state *state,
                    const struct variation_case *variation,
                    struct image_error *error)
{
    const struct variations_options *o = state->options;
    char name[32], log_name[48], report_name[64], image_name[48];
    snprintf(name, sizeof(name), "case-%u", variation->index);
    snprintf(log_name, sizeof(log_name), "%s.log", name);
    snprintf(report_name, sizeof(report_name), "%s.size-report.json", name);
    snprintf(image_name, sizeof(image_name), "%s.img", name);
    char *log = path_join(o->output_directory, log_name);
    char *stage = path_join(state->work, "stage.img");
    char *compact = path_join(state->work, "compact.img");
    char *payload = path_join(state->work, PAYLOAD_DIRECTORY);
    char *after = path_join(state->work, "packages-after.tsv");
    char *raw = path_join(state->work, "extracted.raw");
    char *reference = path_join(state->work, "reference.raw");
    char *size_report = path_join(o->output_directory, report_name);
    char *target = path_join(o->output_directory, image_name);
    char *slim = path_join(o->source_root, "guest/image/slim-guest.sh");
    char *copy_in = NULL, *slim_upload = NULL;
    struct json_buffer packages = { 0 };
    struct json_value *variant = NULL;
    int rc = -1;
    if (!log || !stage || !compact || !payload || !after || !raw ||
        !reference || !size_report || !target || !slim) {
        image_fail(error, "out of memory");
        goto done;
    }
    size_t copy_length = strlen(payload) + sizeof(":/root");
    copy_in = malloc(copy_length);
    size_t slim_length = strlen(slim) + sizeof(":/root/hamn-slim.sh");
    slim_upload = malloc(slim_length);
    if (!copy_in || !slim_upload) {
        image_fail(error, "out of memory");
        goto done;
    }
    snprintf(copy_in, copy_length, "%s:/root", payload);
    snprintf(slim_upload, slim_length, "%s:/root/hamn-slim.sh", slim);

    char *convert[] = { "qemu-img", "convert", "-q", "-f", "qcow2", "-O",
                        "qcow2", (char *)o->baseline, stage, NULL };
    if (run_command(convert, log, COMMAND_TIMEOUT_SECONDS, NULL, error) != 0)
        goto done;
    if (mkdir(payload, 0700) != 0) {
        image_fail_errno(error, "cannot create payload directory", payload);
        goto done;
    }
    if (variations_write_payload(variation, payload, error) != 0)
        goto done;
    char *customize[] = {
        "virt-customize", "-a", stage, "--copy-in", copy_in,
        "--run-command", (char *)variations_guest_install_command,
        "--upload", slim_upload,
        "--run-command", "bash /root/hamn-slim.sh && rm /root/hamn-slim.sh",
        NULL,
    };
    if (run_command(customize, log, COMMAND_TIMEOUT_SECONDS, NULL, error) != 0)
        goto done;
    if (remove_tree(payload) != 0) {
        image_fail_errno(error, "cannot remove payload directory", payload);
        goto done;
    }
    /* The builder's sequence: a fresh journal holds only zeroed blocks. */
    char *trim[] = { "guestfish", "--rw", "add-drive", stage, "format:qcow2",
                     "discard:enable", ":", "run", ":", "debug", "sh",
                     (char *)JOURNAL_RESET, ":", "e2fsck-f", "/dev/sda3", ":",
                     "mount", "/dev/sda3", "/", ":", "fstrim", "/", ":",
                     "umount-all", ":", "shutdown", NULL };
    if (run_command(trim, log, COMMAND_TIMEOUT_SECONDS, NULL, error) != 0)
        goto done;
    char *query[] = { "guestfish", "--ro", "--format=qcow2", "-a", stage, "-i",
                      "command",
                      "dpkg-query -W -f=${Package}\\t${Version}\\t${Installed-Size}\\n",
                      NULL };
    if (run_command(query, log, INVENTORY_TIMEOUT_SECONDS, &packages,
                    error) != 0 ||
        image_write_file(after, packages.data ? packages.data : "",
                         packages.length, error) != 0 ||
        variations_verify_inventory(after,
                                    json_object_get(state->report,
                                                    "packagesAfter"),
                                    error) != 0)
        goto done;
    char *compress[] = { "qemu-img", "convert", "-q", "-f", "qcow2", "-O",
                         "qcow2", "-o", "compression_type=zlib", "-c", stage,
                         compact, NULL };
    char *compare[] = { "qemu-img", "compare", "-q", "-f", "qcow2", "-F",
                        "qcow2", stage, compact, NULL };
    char *decode[] = { state->decoder, compact, raw, NULL };
    char *expand[] = { "qemu-img", "convert", "-q", "-f", "qcow2", "-O", "raw",
                       compact, reference, NULL };
    if (run_command(compress, log, COMMAND_TIMEOUT_SECONDS, NULL, error) != 0 ||
        run_command(compare, log, COMMAND_TIMEOUT_SECONDS, NULL, error) != 0 ||
        run_command(decode, log, COMMAND_TIMEOUT_SECONDS, NULL, error) != 0 ||
        run_command(expand, log, COMMAND_TIMEOUT_SECONDS, NULL, error) != 0)
        goto done;
    char raw_sha[SHA256_HEX_CAP], reference_sha[SHA256_HEX_CAP];
    if (sha256_file(raw, raw_sha, error) != 0 ||
        sha256_file(reference, reference_sha, error) != 0)
        goto done;
    if (strcmp(raw_sha, reference_sha) != 0) {
        image_fail(error, "generated image decoder differs from qemu-img");
        goto done;
    }
    char *raw_command[] = { "hamn-image-tool", "verify-raw", raw, NULL };
    if (log_command(log, raw_command, error) != 0 || raw_check(raw, error) != 0)
        goto done;
    struct size_gate_options gate = {
        .baseline = o->baseline,
        .candidate = compact,
        .packages_before = state->packages_before,
        .packages_after = after,
        .report = size_report,
        .budget = state->budget,
        .base_sha256 = state->base_sha256,
        .source_revision = state->source_revision,
        .review_only = 1,
    };
    char *size_command[] = { "hamn-image-tool", "verify-size", "--review-only",
                             "--candidate", compact, "--report", size_report,
                             NULL };
    if (log_command(log, size_command, error) != 0 ||
        size_gate_run(&gate, error) != 0)
        goto done;
    if (link(compact, target) != 0) {
        image_fail_errno(error, "cannot publish variation image", target);
        goto done;
    }
    if (unlink(compact) != 0) {
        image_fail_errno(error, "cannot remove staged variation image", compact);
        goto done;
    }
    char image_sha[SHA256_HEX_CAP];
    if (sha256_file(target, image_sha, error) != 0)
        goto done;
    variant = json_new_container(JSON_OBJECT);
    if (!variant ||
        add(variant, "case", json_new_integer(variation->index)) != 0 ||
        add(variant, "seed", json_new_integer(variation->seed)) != 0 ||
        add(variant, "regenerableBytesPerFile",
            json_new_integer(variation->regenerable_bytes)) != 0 ||
        add(variant, "image", json_new_cstring(image_name)) != 0 ||
        add(variant, "imageSha256", json_new_cstring(image_sha)) != 0 ||
        add(variant, "rawSha256", json_new_cstring(raw_sha)) != 0 ||
        add(variant, "sizeReport", json_new_cstring(report_name)) != 0 ||
        add(variant, "structuralChecks", json_new_cstring("passed")) != 0 ||
        add(variant, "physicalRuntimeValidation",
            json_new_cstring("pending")) != 0 ||
        json_array_append(state->variants, variant) != 0) {
        image_fail(error, "out of memory");
        goto done;
    }
    variant = NULL;
    if (unlink(stage) != 0 || unlink(raw) != 0 || unlink(reference) != 0) {
        image_fail_errno(error, "cannot remove case intermediates", state->work);
        goto done;
    }
    rc = 0;

done:
    json_free(variant);
    json_buffer_free(&packages);
    free(log);
    free(stage);
    free(compact);
    free(payload);
    free(after);
    free(raw);
    free(reference);
    free(size_report);
    free(target);
    free(slim);
    free(copy_in);
    free(slim_upload);
    return rc;
}

static int load_report(struct run_state *state, struct image_error *error)
{
    const struct variations_options *o = state->options;
    struct stat baseline_info, report_info;
    if (!image_owned_single_link(o->baseline, &baseline_info) ||
        !image_owned_single_link(o->size_report, &report_info))
        return image_fail(error, "unsafe baseline evidence source");
    state->report = image_read_json(o->size_report, error);
    if (!state->report)
        return -1;
    const struct json_value *report = state->report;
    const struct json_value *base = json_object_get(report, "baseImageSha256");
    const struct json_value *revision = json_object_get(report, "sourceRevision");
    if (report->type != JSON_OBJECT || !image_json_lower_hex(base, 64) ||
        !image_json_lower_hex(revision, 40) ||
        !nonempty_string_array(json_object_get(report, "packagesBefore")) ||
        !nonempty_string_array(json_object_get(report, "packagesAfter")))
        return image_fail(error, "size report lacks the build identity or package inventories");
    state->base_sha256 = base->text;
    state->source_revision = revision->text;
    char *baseline_sha = state->baseline_sha;
    if (sha256_file(o->baseline, baseline_sha, error) != 0)
        return -1;
    long long bytes = 0;
    if (json_integer_value(json_object_get(report, "baselineCompressedBytes"),
                           &bytes) != 0 ||
        bytes != (long long)baseline_info.st_size ||
        !json_string_equals(json_object_get(report, "baselineSha256"),
                            baseline_sha))
        return image_fail(error,
            "exported baseline does not match the actual build report");
    return 0;
}

static int prepare_output(struct run_state *state, struct image_error *error)
{
    const struct variations_options *o = state->options;
    char *copy = strdup(o->output_directory);
    if (!copy)
        return image_fail(error, "out of memory");
    size_t length = strlen(copy);
    while (length > 1 && copy[length - 1] == '/')
        copy[--length] = '\0';
    char *slash = strrchr(copy, '/');
    const char *parent = slash ? (slash == copy ? "/" : copy) : ".";
    if (slash && slash != copy)
        *slash = '\0';
    struct stat info;
    int safe = lstat(parent, &info) == 0 && S_ISDIR(info.st_mode) &&
               info.st_uid == geteuid() && (info.st_mode & 0022) == 0;
    free(copy);
    if (!safe)
        return image_fail(error, "unsafe variation output parent");
    /* Exclusive directory ownership prevents overwriting previous evidence. */
    if (mkdir(o->output_directory, 0700) != 0)
        return image_fail_errno(error, "cannot create variation output",
                                o->output_directory);
    return 0;
}

static int source_revision_of(const char *root, char revision[41],
                              struct image_error *error)
{
    char *command[] = { "git", "-C", (char *)root, "rev-parse", "HEAD", NULL };
    struct json_buffer output = { 0 };
    int rc = run_command(command, NULL, GIT_TIMEOUT_SECONDS, &output, error);
    if (rc == 0) {
        size_t length = output.length;
        while (length > 0 && (output.data[length - 1] == '\n' ||
                              output.data[length - 1] == ' '))
            length--;
        if (!image_lower_hex(output.data, length, 40))
            rc = image_fail(error, "git returned an invalid source revision");
        else
            memcpy(revision, output.data, 40);
        revision[40] = '\0';
    }
    json_buffer_free(&output);
    return rc;
}

static int generate(struct run_state *state,
                    const struct variation_case *cases, size_t count,
                    struct image_error *error)
{
    const struct variations_options *o = state->options;
    char revision[41];
    char report_sha[SHA256_HEX_CAP];
    const char *baseline_sha = state->baseline_sha;
    if (sha256_file(o->size_report, report_sha, error) != 0 ||
        source_revision_of(o->source_root, revision, error) != 0)
        return -1;
    state->evidence = json_new_container(JSON_OBJECT);
    state->variants = json_new_container(JSON_ARRAY);
    if (!state->evidence || !state->variants ||
        add(state->evidence, "schemaVersion", json_new_integer(1)) != 0 ||
        add(state->evidence, "seed", json_new_integer(o->seed)) != 0 ||
        add(state->evidence, "baselineSha256",
            json_new_cstring(baseline_sha)) != 0 ||
        add(state->evidence, "baselineSizeReportSha256",
            json_new_cstring(report_sha)) != 0 ||
        add(state->evidence, "sourceRevision", json_new_cstring(revision)) != 0 ||
        add(state->evidence, "physicalRuntimeValidation",
            json_new_cstring("pending")) != 0)
        return image_fail(error, "out of memory");

    char *template = path_join(o->output_directory, ".variation-stage-XXXXXX");
    if (!template || !mkdtemp(template)) {
        free(template);
        return image_fail_errno(error, "cannot create variation stage",
                                o->output_directory);
    }
    state->work = template;
    state->decoder = path_join(state->work, "extract-check");
    state->packages_before = path_join(state->work, "packages-before.tsv");
    state->budget = path_join(o->source_root,
                              "guest/image/release-size-budget.json");
    char *build_log = path_join(o->output_directory, "decoder-build.log");
    char *host_include = NULL, *extract = path_join(o->source_root,
                                                    "guest/image/extract-check.c");
    char *qcow2 = path_join(o->source_root, "host/image/qcow2.c");
    char *host = path_join(o->source_root, "host");
    if (host) {
        host_include = malloc(strlen(host) + 3);
        if (host_include)
            snprintf(host_include, strlen(host) + 3, "-I%s", host);
    }
    int rc = -1;
    if (!state->decoder || !state->packages_before || !state->budget ||
        !build_log || !extract || !qcow2 || !host_include) {
        image_fail(error, "out of memory");
        goto done;
    }
    char *compile[] = { "cc", "-D_GNU_SOURCE", "-std=c11", "-O2", "-Wall",
                        "-Wextra", "-Werror=implicit-function-declaration",
                        host_include, extract, qcow2, "-lz", "-o",
                        state->decoder, NULL };
    if (run_command(compile, build_log, COMMAND_TIMEOUT_SECONDS, NULL,
                    error) != 0 ||
        write_packages_before(state, error) != 0)
        goto done;
    for (size_t index = 0; index < count; index++) {
        if (run_case(state, &cases[index], error) != 0)
            goto done;
    }
    char current[SHA256_HEX_CAP];
    if (sha256_file(o->baseline, current, error) != 0)
        goto done;
    if (strcmp(current, baseline_sha) != 0) {
        image_fail(error, "input baseline changed during variation generation");
        goto done;
    }
    if (json_object_set(state->evidence, "variants", state->variants) != 0) {
        image_fail(error, "out of memory");
        goto done;
    }
    state->variants = NULL;
    char *index_path = path_join(o->output_directory, "variations.json");
    if (!index_path) {
        image_fail(error, "out of memory");
        goto done;
    }
    rc = image_write_json(index_path, state->evidence, 0, error);
    free(index_path);

done:
    free(build_log);
    free(host_include);
    free(extract);
    free(qcow2);
    free(host);
    return rc;
}

int variations_run(const struct variations_options *options,
                   struct image_error *error)
{
    static const char *const dependencies[] = {
        "qemu-img", "virt-customize", "guestfish", "cc", "git",
    };
    struct variation_case cases[VARIATION_MAX_CASES];
    if (variations_plan(options->seed, options->case_count, cases, error) != 0)
        return -1;
    struct utsname host;
    if (uname(&host) != 0)
        return image_fail(error, "cannot identify the host: %s", strerror(errno));
#ifdef HAMN_TEST
    /* Test builds only: drive the orchestration with fake builder tools. */
    if (getenv("HAMN_TEST_VARIATIONS_HOST")) {
        snprintf(host.sysname, sizeof(host.sysname), "Linux");
        snprintf(host.machine, sizeof(host.machine), "aarch64");
    }
#endif
    if (strcmp(host.sysname, "Linux") != 0 ||
        (strcmp(host.machine, "aarch64") != 0 &&
         strcmp(host.machine, "arm64") != 0))
        return image_fail(error, "real image variations require Linux arm64");
    for (size_t index = 0;
         index < sizeof(dependencies) / sizeof(dependencies[0]); index++) {
        if (!executable_in_path(dependencies[index]))
            return image_fail(error, "missing image builder dependency: %s",
                              dependencies[index]);
    }
    const char *paths[] = { options->source_root, options->baseline,
                            options->size_report, options->output_directory };
    for (size_t index = 0; index < sizeof(paths) / sizeof(paths[0]); index++) {
        if (!json_utf8_valid(paths[index], strlen(paths[index])))
            return image_fail(error, "paths must be UTF-8: %s", paths[index]);
    }

    struct run_state state = { .options = options };
    struct sigaction previous[GUARDED_SIGNAL_COUNT];
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = record_signal;
    sigemptyset(&action.sa_mask);
    interrupted_signal = 0;
    for (size_t index = 0; index < GUARDED_SIGNAL_COUNT; index++)
        sigaction(guarded_signals[index], &action, &previous[index]);

    int rc = -1;
    if (load_report(&state, error) == 0 && prepare_output(&state, error) == 0)
        rc = generate(&state, cases, (size_t)options->case_count, error);
    if (state.work && remove_tree(state.work) != 0 && rc == 0)
        rc = image_fail_errno(error, "cannot remove variation stage", state.work);
    for (size_t index = 0; index < GUARDED_SIGNAL_COUNT; index++)
        sigaction(guarded_signals[index], &previous[index], NULL);
    if (rc == 0)
        printf("Generated %lld structurally validated images; physical runtime "
               "validation remains pending: %s\n",
               options->case_count, options->output_directory);
    json_free(state.report);
    json_free(state.evidence);
    json_free(state.variants);
    free(state.work);
    free(state.decoder);
    free(state.packages_before);
    free(state.budget);
    return rc;
}
