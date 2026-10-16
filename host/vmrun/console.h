#ifndef HAMN_VMRUN_CONSOLE_H
#define HAMN_VMRUN_CONSOLE_H

#include <stddef.h>

/*
 * Guest console relay of vmrun.
 *
 * Virtualization.framework writes the guest's virtio console (hvc0) into a
 * pipe that vmrun owns. The relay copies every byte, unchanged and in order,
 * to the profile's serial log, and watches the same bytes for the Linux
 * kernel's panic banner. The guest image boots with `oops=panic panic=0`, so
 * a damaged kernel panics and then waits; vmrun stops the VM after the banner
 * (cmd_vmrun.c has the timing; a reboot would restart the guest inside the
 * same VM, unseen by Hamn).
 *
 * The banner is the kernel's own text, printed on every console:
 *   "Kernel panic - not syncing: <reason>"            starts the panic
 *   "---[ end Kernel panic - not syncing: <reason> ]---"  ends its report
 * The match is byte-exact and case-sensitive anywhere in the stream (the
 * printk timestamp in front of it varies), across any split of the stream
 * into reads. Only root in the guest can write such a line to hvc0; a false
 * match stops the VM, nothing else.
 *
 * The reason is the rest of the start banner's line: the bytes after
 * "Kernel panic - not syncing" up to the next CR or LF, without a leading
 * ": ", at most CONSOLE_PANIC_REASON_CAP - 1 bytes. Bytes outside printable
 * ASCII become '?', so the reason is always valid ASCII text.
 */

#define CONSOLE_PANIC_REASON_CAP 192

enum console_panic_stage {
    CONSOLE_QUIET = 0,     /* no panic banner seen */
    CONSOLE_PANIC_STARTED, /* "Kernel panic - not syncing" seen */
    CONSOLE_PANIC_ENDED,   /* then "---[ end Kernel panic - not syncing" */
};

/* Streaming matcher state; zero-initialized or console_scan_init. */
struct console_scan {
    size_t start_matched; /* bytes of the start banner matched so far */
    size_t end_matched;   /* bytes of the end banner matched so far */
    enum console_panic_stage stage;
    int reason_open;      /* the start banner's line is still being read */
    size_t reason_length;
    char reason[CONSOLE_PANIC_REASON_CAP]; /* NUL-terminated */
};

void console_scan_init(struct console_scan *scan);

/*
 * Feeds length bytes (any values, NUL included) and returns the stage after
 * them. The stage only moves forward; an end banner counts only after a start
 * banner.
 */
enum console_panic_stage console_scan_feed(struct console_scan *scan,
                                           const void *bytes, size_t length);

/* The reason read so far ("" before the start banner); complete once the
 * banner's line has ended. Valid until the next feed. */
const char *console_scan_reason(const struct console_scan *scan);

/*
 * Called on the relay's own serial dispatch queue, once when the stage
 * becomes CONSOLE_PANIC_STARTED and once when it becomes
 * CONSOLE_PANIC_ENDED, in that order (both from one read when one read
 * carries both banners), with the reason as console_scan_reason has it then
 * (valid during the call only; at CONSOLE_PANIC_ENDED the start banner's
 * line has ended, so the reason is complete). It must not block for long:
 * the guest's console output waits behind it.
 */
typedef void (*console_panic_fn)(enum console_panic_stage stage,
                                 const char *reason, void *context);

/*
 * Starts relaying read_fd (the read end of the console pipe) to log_fd (the
 * serial log, opened for writing). Takes ownership of both descriptors:
 * from this call on, the relay closes them at end of input or on a read
 * error, and also when it cannot start (-1). read_fd is made non-blocking.
 *
 * A failed write to the serial log is reported once through logerr and does
 * not stop the relay: the console is still read (so the guest never blocks
 * on a full pipe) and still watched. 0 = started.
 */
int console_relay_start(int read_fd, int log_fd, console_panic_fn on_panic,
                        void *context);

#endif
