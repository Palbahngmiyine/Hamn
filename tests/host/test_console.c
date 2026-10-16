/* The guest console relay of vmrun: the panic banner matcher and the copy to
 * the serial log.
 *
 * The matcher is checked against an oracle derived from its contract
 * (memmem over the bytes fed so far), for every prefix and every split of a
 * console excerpt that a real guest printed when its kernel panicked. The
 * relay tests write into a real pipe and wait on the relay's own callbacks
 * and on its end of input; FAILURE_DEADLINE_SEC only turns a hang into a
 * failure. No test orders events by sleeping. */
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include <dispatch/dispatch.h>

#include "vmrun/console.h"

#define FAILURE_DEADLINE_SEC 10

static const char START[] = "Kernel panic - not syncing";
static const char END[] = "---[ end Kernel panic - not syncing";

/* From the hvc0 log of a guest whose kernel stack was damaged (2026-10-15). */
static const char PANIC_EXCERPT[] =
    "[  131.915199] hamn-hb up=132.30 load=0.06\r\n"
    "[  133.956960] Kernel panic - not syncing: corrupted stack end detected "
    "inside scheduler\r\n"
    "[  133.957569] CPU: 1 PID: 116 Comm: kworker/1:1H Not tainted "
    "6.8.0-139-generic #139-Ubuntu\r\n"
    "[  133.984105] Call trace:\r\n"
    "[  133.985450]  panic+0x3ac/0x448\r\n"
    "[  133.994879] SMP: stopping secondary CPUs\r\n"
    "[  134.017708] ---[ end Kernel panic - not syncing: corrupted stack end "
    "detected inside scheduler ]---\r\n";

static const char BOOT_EXCERPT[] =
    "[    0.000000] Kernel command line: BOOT_IMAGE=/vmlinuz-6.8.0-139-generic "
    "ro console=tty1 console=ttyAMA0 console=hvc0 fsck.repair=yes "
    "oops=panic panic=0\r\n"
    "[    5.144893] sysrq: Show Blocked State\r\n"
    "[   97.831371] WARNING: CPU: 2 PID: 1636 at fs/kernfs/dir.c:154\r\n"
    "Ubuntu 24.04.4 LTS hamn hvc0\r\n\r\nhamn login: ";

static void fail(const char *message)
{
    perror(message);
    exit(1);
}

static void require(int condition, const char *message)
{
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", message);
        exit(1);
    }
}

/* The stage the contract assigns to the first length bytes of text. */
static enum console_panic_stage oracle(const char *text, size_t length)
{
    const char *start = memmem(text, length, START, strlen(START));
    if (!start)
        return CONSOLE_QUIET;
    const char *after = start + strlen(START);
    size_t rest = length - (size_t)(after - text);
    return memmem(after, rest, END, strlen(END)) ? CONSOLE_PANIC_ENDED
                                                  : CONSOLE_PANIC_STARTED;
}

static enum console_panic_stage scan_whole(const char *text, size_t length)
{
    struct console_scan scan;
    console_scan_init(&scan);
    return console_scan_feed(&scan, text, length);
}

static void test_every_prefix_matches_the_oracle(const char *text)
{
    size_t length = strlen(text);
    for (size_t k = 0; k <= length; k++)
        require(scan_whole(text, k) == oracle(text, k),
                "a prefix's stage differs from the contract");
}

static void test_every_split_matches_the_whole(const char *text)
{
    size_t length = strlen(text);
    enum console_panic_stage whole = scan_whole(text, length);
    for (size_t split = 0; split <= length; split++) {
        struct console_scan scan;
        console_scan_init(&scan);
        enum console_panic_stage first =
            console_scan_feed(&scan, text, split);
        require(first == oracle(text, split),
                "the first part of a split has the wrong stage");
        require(console_scan_feed(&scan, text + split, length - split) ==
                    whole,
                "a split stream ends in another stage than the whole");
    }
    struct console_scan scan;
    console_scan_init(&scan);
    for (size_t i = 0; i < length; i++) {
        enum console_panic_stage stage = console_scan_feed(&scan, text + i, 1);
        require(stage == oracle(text, i + 1),
                "a byte-at-a-time stream has the wrong stage");
    }
}

static void test_real_panic_is_started_then_ended(void)
{
    require(scan_whole(PANIC_EXCERPT, strlen(PANIC_EXCERPT)) ==
                CONSOLE_PANIC_ENDED,
            "a real panic report must end in CONSOLE_PANIC_ENDED");
    const char *end_line = strstr(PANIC_EXCERPT, "[  134.017708]");
    require(end_line != NULL, "excerpt fixture");
    require(scan_whole(PANIC_EXCERPT, (size_t)(end_line - PANIC_EXCERPT)) ==
                CONSOLE_PANIC_STARTED,
            "a panic before its end line must be CONSOLE_PANIC_STARTED");
    test_every_prefix_matches_the_oracle(PANIC_EXCERPT);
    test_every_split_matches_the_whole(PANIC_EXCERPT);
}

static void test_boot_output_stays_quiet(void)
{
    require(scan_whole(BOOT_EXCERPT, strlen(BOOT_EXCERPT)) == CONSOLE_QUIET,
            "boot output with panic=0 and oops=panic must stay quiet");
    static const char *const near_misses[] = {
        "Kernel panic - not syncinG: x\n",
        "kernel panic - not syncing: x\n",
        "Kernel panic -  not syncing: x\n",
        "Kernel panic\n",
        "Kernel panic - not syncin",
    };
    for (size_t i = 0; i < sizeof(near_misses) / sizeof(near_misses[0]); i++)
        require(scan_whole(near_misses[i], strlen(near_misses[i])) ==
                    CONSOLE_QUIET,
                "a near miss of the banner must stay quiet");
}

static void test_partial_matches_restart_correctly(void)
{
    static const char *const texts[] = {
        "Kernel Kernel panic - not syncing: x\n",
        "KKernel panic - not syncing: x\n",
        "Kernel panic - not Kernel panic - not syncing: x\n",
        "Kernel panic - not syncing: x\n----[ end Kernel panic - not syncing\n",
        "Kernel panic - not syncing: x\n---[ ---[ end Kernel panic - not "
        "syncing\n",
        "Kernel panic - not syncing: x\n---[ end Kernel panic - not ---[ end "
        "Kernel panic - not syncing\n",
    };
    for (size_t i = 0; i < sizeof(texts) / sizeof(texts[0]); i++) {
        require(scan_whole(texts[i], strlen(texts[i])) ==
                    oracle(texts[i], strlen(texts[i])),
                "a restart inside a partial match lost the banner");
        test_every_split_matches_the_whole(texts[i]);
    }
    require(scan_whole(texts[3], strlen(texts[3])) == CONSOLE_PANIC_ENDED,
            "an end banner after extra dashes must end the panic");
}

static void test_end_banner_counts_only_after_a_start(void)
{
    static const char alone[] = "---[ end ";
    require(scan_whole(alone, strlen(alone)) == CONSOLE_QUIET,
            "the end banner's opening alone must stay quiet");
    /* The end line repeats the start banner: alone, it starts a panic. */
    static const char end_line[] =
        "---[ end Kernel panic - not syncing: x ]---\n";
    require(scan_whole(end_line, strlen(end_line)) == CONSOLE_PANIC_STARTED,
            "an end line without a start line only starts the panic");
    struct console_scan scan;
    console_scan_init(&scan);
    require(console_scan_feed(&scan, end_line, strlen(end_line)) ==
                CONSOLE_PANIC_STARTED,
            "first end line");
    require(console_scan_feed(&scan, end_line, strlen(end_line)) ==
                CONSOLE_PANIC_ENDED,
            "a second end line after the start ends the panic");
    require(console_scan_feed(&scan, BOOT_EXCERPT, strlen(BOOT_EXCERPT)) ==
                CONSOLE_PANIC_ENDED,
            "the stage never moves back");
}

static void test_nul_bytes_are_plain_bytes(void)
{
    static const char text[] = "\0\0Kernel panic - not syncing\0: x";
    require(scan_whole(text, sizeof(text) - 1) == CONSOLE_PANIC_STARTED,
            "NUL bytes around the banner must not hide it");
    static const char broken[] = "Kernel panic\0 - not syncing";
    require(scan_whole(broken, sizeof(broken) - 1) == CONSOLE_QUIET,
            "a NUL inside the banner breaks it");
}

/* Written on the relay queue, read by the test after the relay closed. */
struct observed {
    dispatch_semaphore_t ended;
    enum console_panic_stage stages[4];
    atomic_int count;
};

static void on_panic(enum console_panic_stage stage, void *context)
{
    struct observed *observed = context;
    int index = atomic_load(&observed->count);
    if (index < 4)
        observed->stages[index] = stage;
    atomic_fetch_add(&observed->count, 1);
    if (stage == CONSOLE_PANIC_ENDED)
        dispatch_semaphore_signal(observed->ended);
}

struct fd_identity {
    int fd;
    dev_t dev;
    ino_t ino;
};

static struct fd_identity fd_identity(int fd)
{
    struct stat sb;
    if (fstat(fd, &sb) != 0)
        fail("fstat");
    return (struct fd_identity){ fd, sb.st_dev, sb.st_ino };
}

/* Whether the descriptor still refers to the same open file (its number may
 * have been reused by another open since it was closed). */
static int fd_still_open(struct fd_identity id)
{
    struct stat sb;
    return fstat(id.fd, &sb) == 0 && sb.st_dev == id.dev &&
           sb.st_ino == id.ino;
}

/* Waits until the relay has closed both descriptors (its end of input). */
static void wait_relay_closed(struct fd_identity read_end,
                              struct fd_identity log_end)
{
    struct timespec started, now;
    clock_gettime(CLOCK_MONOTONIC, &started);
    while (fd_still_open(read_end) || fd_still_open(log_end)) {
        clock_gettime(CLOCK_MONOTONIC, &now);
        require(now.tv_sec - started.tv_sec < FAILURE_DEADLINE_SEC,
                "the relay must close its descriptors at end of input");
        sched_yield();
    }
}

static void write_all(int fd, const void *bytes, size_t length)
{
    const char *p = bytes;
    while (length > 0) {
        ssize_t n = write(fd, p, length);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            fail("write");
        p += n;
        length -= (size_t)n;
    }
}

static char *read_file(const char *path, size_t *length)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        fail("open log");
    struct stat sb;
    if (fstat(fd, &sb) != 0)
        fail("fstat log");
    char *data = malloc((size_t)sb.st_size + 1);
    if (!data)
        fail("malloc");
    size_t got = 0;
    while (got < (size_t)sb.st_size) {
        ssize_t n = read(fd, data + got, (size_t)sb.st_size - got);
        if (n <= 0)
            fail("read log");
        got += (size_t)n;
    }
    close(fd);
    *length = got;
    return data;
}

/* Relays stream in pieces of piece bytes; returns the observed callbacks. */
static struct observed relay_stream(const char *log_path, int log_flags,
                                    const char *stream, size_t length,
                                    size_t piece)
{
    int pipe_fds[2];
    if (pipe(pipe_fds) != 0)
        fail("pipe");
    int log_fd = open(log_path, log_flags, 0644);
    if (log_fd < 0)
        fail("open log for the relay");
    struct fd_identity read_end = fd_identity(pipe_fds[0]);
    struct fd_identity log_end = fd_identity(log_fd);
    struct observed observed = { .ended = dispatch_semaphore_create(0) };
    require(console_relay_start(pipe_fds[0], log_fd, on_panic, &observed) ==
                0,
            "the relay must start");
    for (size_t offset = 0; offset < length; offset += piece) {
        size_t n = length - offset < piece ? length - offset : piece;
        write_all(pipe_fds[1], stream + offset, n);
    }
    close(pipe_fds[1]);
    wait_relay_closed(read_end, log_end);
    dispatch_release(observed.ended);
    return observed;
}

static void test_relay_copies_every_byte_and_reports_once(const char *dir)
{
    char log_path[1024];
    snprintf(log_path, sizeof(log_path), "%s/serial.log", dir);
    /* A large quiet prefix with NUL bytes, so the pipe fills and the relay
     * has to keep up, then the panic. */
    size_t quiet = 1 << 20;
    size_t length = quiet + strlen(PANIC_EXCERPT);
    char *stream = malloc(length);
    if (!stream)
        fail("malloc");
    for (size_t i = 0; i < quiet; i++)
        stream[i] = (char)(i % 251);
    memcpy(stream + quiet, PANIC_EXCERPT, strlen(PANIC_EXCERPT));
    require(oracle(stream, quiet) == CONSOLE_QUIET, "quiet fixture");

    static const size_t pieces[] = { 1, 7, 4096, 65536 };
    for (size_t p = 0; p < sizeof(pieces) / sizeof(pieces[0]); p++) {
        size_t piece = pieces[p];
        const char *data = stream;
        size_t size = length;
        if (piece == 1) {
            /* Byte by byte only over the panic report, to keep it short. */
            data = PANIC_EXCERPT;
            size = strlen(PANIC_EXCERPT);
        }
        struct observed observed = relay_stream(
            log_path, O_WRONLY | O_CREAT | O_TRUNC, data, size, piece);
        require(observed.count == 2, "two callbacks for one panic");
        require(observed.stages[0] == CONSOLE_PANIC_STARTED &&
                    observed.stages[1] == CONSOLE_PANIC_ENDED,
                "the start is reported before the end");
        size_t logged = 0;
        char *log = read_file(log_path, &logged);
        require(logged == size && memcmp(log, data, size) == 0,
                "the serial log must hold the console bytes unchanged");
        free(log);
    }
    require(unlink(log_path) == 0, "remove log");
    free(stream);
}

static void test_relay_keeps_watching_when_the_log_fails(const char *dir)
{
    char log_path[1024];
    snprintf(log_path, sizeof(log_path), "%s/readonly.log", dir);
    int fd = open(log_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0)
        fail("create log");
    close(fd);
    /* Opened read-only, every write to the serial log fails. */
    struct observed observed = relay_stream(log_path, O_RDONLY, PANIC_EXCERPT,
                                            strlen(PANIC_EXCERPT), 64);
    require(observed.count == 2 &&
                observed.stages[1] == CONSOLE_PANIC_ENDED,
            "a failing serial log must not hide the panic");
    require(unlink(log_path) == 0, "remove log");
}

static void test_relay_quiet_stream_reports_nothing(const char *dir)
{
    char log_path[1024];
    snprintf(log_path, sizeof(log_path), "%s/quiet.log", dir);
    struct observed observed =
        relay_stream(log_path, O_WRONLY | O_CREAT | O_TRUNC, BOOT_EXCERPT,
                     strlen(BOOT_EXCERPT), 16);
    require(observed.count == 0, "boot output must not report a panic");
    require(unlink(log_path) == 0, "remove log");
}

static void test_relay_start_failure_closes_both(const char *dir)
{
    char log_path[1024];
    snprintf(log_path, sizeof(log_path), "%s/unused.log", dir);
    int log_fd = open(log_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (log_fd < 0)
        fail("open log");
    struct fd_identity log_end = fd_identity(log_fd);
    struct observed observed = { 0 };
    require(console_relay_start(-1, log_fd, on_panic, &observed) == -1,
            "an invalid console descriptor must fail");
    require(!fd_still_open(log_end), "a failed start must close the serial log");
    require(unlink(log_path) == 0, "remove log");
}

int main(void)
{
    test_real_panic_is_started_then_ended();
    test_boot_output_stays_quiet();
    test_partial_matches_restart_correctly();
    test_end_banner_counts_only_after_a_start();
    test_nul_bytes_are_plain_bytes();

    char directory[] = "/tmp/hamn-console-test-XXXXXX";
    if (!mkdtemp(directory))
        fail("mkdtemp");
    test_relay_copies_every_byte_and_reports_once(directory);
    test_relay_keeps_watching_when_the_log_fails(directory);
    test_relay_quiet_stream_reports_nothing(directory);
    test_relay_start_failure_closes_both(directory);
    require(rmdir(directory) == 0, "fixture directory must be empty");
    puts("console tests: ok");
    return 0;
}
