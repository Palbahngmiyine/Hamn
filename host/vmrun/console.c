#include "vmrun/console.h"

#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <dispatch/dispatch.h>

#include "core/log.h"

static const char START_BANNER[] = "Kernel panic - not syncing";
static const char END_BANNER[] = "---[ end Kernel panic - not syncing";

#define START_LENGTH (sizeof(START_BANNER) - 1)
#define END_LENGTH (sizeof(END_BANNER) - 1)

/* Reads per wakeup before the relay yields its queue. */
#define RELAY_READS_PER_EVENT 64

void console_scan_init(struct console_scan *scan)
{
    assert(scan);
    memset(scan, 0, sizeof(*scan));
}

/* Longest proper prefix of pattern[0, n) that is also its suffix. */
static size_t border(const char *pattern, size_t n)
{
    for (size_t k = n - 1; k > 0; k--) {
        if (memcmp(pattern, pattern + n - k, k) == 0)
            return k;
    }
    return 0;
}

/* Matched length of pattern after byte c, given matched bytes before it. */
static size_t step(const char *pattern, size_t matched, unsigned char c)
{
    while (matched > 0 && (unsigned char)pattern[matched] != c)
        matched = border(pattern, matched);
    if ((unsigned char)pattern[matched] == c)
        matched++;
    return matched;
}

enum console_panic_stage console_scan_feed(struct console_scan *scan,
                                           const void *bytes, size_t length)
{
    assert(scan);
    assert(bytes || length == 0);
    assert(scan->start_matched < START_LENGTH);
    assert(scan->end_matched < END_LENGTH);
    assert(scan->reason_length < CONSOLE_PANIC_REASON_CAP);
    const unsigned char *p = bytes;
    for (size_t i = 0; i < length && scan->stage != CONSOLE_PANIC_ENDED; i++) {
        if (scan->stage == CONSOLE_QUIET) {
            scan->start_matched = step(START_BANNER, scan->start_matched, p[i]);
            if (scan->start_matched == START_LENGTH) {
                scan->start_matched = 0;
                scan->stage = CONSOLE_PANIC_STARTED;
                scan->reason_open = 1;
            }
        } else {
            if (scan->reason_open) {
                if (p[i] == '\n' || p[i] == '\r') {
                    scan->reason_open = 0;
                } else if (scan->reason_length < CONSOLE_PANIC_REASON_CAP - 1) {
                    scan->reason[scan->reason_length++] =
                        p[i] >= 0x20 && p[i] < 0x7f ? (char)p[i] : '?';
                    scan->reason[scan->reason_length] = '\0';
                }
            }
            scan->end_matched = step(END_BANNER, scan->end_matched, p[i]);
            if (scan->end_matched == END_LENGTH) {
                scan->end_matched = 0;
                scan->stage = CONSOLE_PANIC_ENDED;
            }
        }
    }
    return scan->stage;
}

const char *console_scan_reason(const struct console_scan *scan)
{
    assert(scan);
    /* The kernel prints "Kernel panic - not syncing: <reason>". */
    const char *reason = scan->reason;
    if (*reason == ':')
        reason++;
    while (*reason == ' ')
        reason++;
    return reason;
}

struct console_relay {
    dispatch_source_t source;
    int read_fd;
    int log_fd;
    int log_failed;
    struct console_scan scan;
    console_panic_fn on_panic;
    void *context;
};

static void relay_write_log(struct console_relay *relay,
                            const unsigned char *bytes, size_t length)
{
    while (length > 0) {
        ssize_t written = write(relay->log_fd, bytes, length);
        if (written > 0) {
            bytes += written;
            length -= (size_t)written;
            continue;
        }
        if (written < 0 && errno == EINTR)
            continue;
        if (!relay->log_failed) {
            logerr("vmrun: cannot write the serial log: %s; the guest "
                   "console is still watched",
                   written < 0 ? strerror(errno) : "no progress");
            relay->log_failed = 1;
        }
        return;
    }
}

static void relay_read(void *context)
{
    struct console_relay *relay = context;
    unsigned char buffer[4096];
    for (int reads = 0; reads < RELAY_READS_PER_EVENT; reads++) {
        ssize_t n = read(relay->read_fd, buffer, sizeof(buffer));
        if (n > 0) {
            relay_write_log(relay, buffer, (size_t)n);
            enum console_panic_stage before = relay->scan.stage;
            enum console_panic_stage after =
                console_scan_feed(&relay->scan, buffer, (size_t)n);
            const char *reason = console_scan_reason(&relay->scan);
            if (before < CONSOLE_PANIC_STARTED &&
                after >= CONSOLE_PANIC_STARTED)
                relay->on_panic(CONSOLE_PANIC_STARTED, reason, relay->context);
            if (before < CONSOLE_PANIC_ENDED && after == CONSOLE_PANIC_ENDED)
                relay->on_panic(CONSOLE_PANIC_ENDED, reason, relay->context);
            continue;
        }
        if (n < 0 && errno == EINTR)
            continue;
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
            return;
        if (n < 0)
            logerr("vmrun: cannot read the guest console: %s",
                   strerror(errno));
        dispatch_source_cancel(relay->source);
        return;
    }
}

static void relay_cancel(void *context)
{
    struct console_relay *relay = context;
    close(relay->read_fd);
    close(relay->log_fd);
    dispatch_release(relay->source);
    free(relay);
}

int console_relay_start(int read_fd, int log_fd, console_panic_fn on_panic,
                        void *context)
{
    assert(on_panic);
    struct console_relay *relay = NULL;
    int flags = read_fd >= 0 ? fcntl(read_fd, F_GETFL) : -1;
    if (read_fd < 0 || log_fd < 0 || flags < 0 ||
        fcntl(read_fd, F_SETFL, flags | O_NONBLOCK) != 0 ||
        !(relay = calloc(1, sizeof(*relay))))
        goto fail;

    dispatch_queue_t queue =
        dispatch_queue_create("dev.hamn.console", DISPATCH_QUEUE_SERIAL);
    if (!queue)
        goto fail;
    relay->source = dispatch_source_create(DISPATCH_SOURCE_TYPE_READ,
                                           (uintptr_t)read_fd, 0, queue);
    /* The source keeps its target queue alive. */
    dispatch_release(queue);
    if (!relay->source)
        goto fail;
    relay->read_fd = read_fd;
    relay->log_fd = log_fd;
    relay->on_panic = on_panic;
    relay->context = context;
    console_scan_init(&relay->scan);
    dispatch_set_context(relay->source, relay);
    dispatch_source_set_event_handler_f(relay->source, relay_read);
    dispatch_source_set_cancel_handler_f(relay->source, relay_cancel);
    dispatch_resume(relay->source);
    return 0;

fail:
    free(relay);
    if (read_fd >= 0)
        close(read_fd);
    if (log_fd >= 0)
        close(log_fd);
    return -1;
}
