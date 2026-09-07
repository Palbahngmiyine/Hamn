#include "core/remote_mutation.h"
#include "core/operation.h"
#include "core/log.h"
#include "sshmgr/ssh.h"
#include "util/proc.h"
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int cleanup_pending;
int remote_mutation_cleanup_pending(void) { return cleanup_pending; }

/* /run is guest-root-owned. Reject replacement of our private marker directory.
 * Markers whose original SSH command vanished remain until reboot; deleting one
 * without its acknowledgement would permit a delayed command to run again. */
#define SECURE_MARKERS \
    "set -eu; umask 077; d=/run/hamn-cancelled-mutations; " \
    "test ! -L \"$d\"; mkdir -p -m 700 \"$d\"; " \
    "test \"$(stat -c '%u:%a' \"$d\")\" = 0:700; "
static const char writer[] = SECURE_MARKERS
    "token=$1; lock=$2; wait=$3; limit=$4; shift 4; "
    "exec flock --wait \"$wait\" \"$lock\" bash -c '"
    "set -eu; token=$1; limit=$2; shift 2; "
    "marker=/run/hamn-cancelled-mutations/$token; "
    "if test -e \"$marker\"; then rm -- \"$marker\"; exit 130; fi; "
    "exec timeout --kill-after=5s \"$limit\" \"$@\"' -- \"$token\" \"$limit\" \"$@\"";
static const char fence[] = SECURE_MARKERS
    "test ! -L \"$d/$1\"; : > \"$d/$1\"";

int remote_mutation_run(const struct profile *profile, const char *ip,
                        const char *lock, unsigned wait_seconds,
                        unsigned run_seconds, const char *const command[],
                        char *output, size_t capacity, int *truncated)
{
    assert(command && command[0] && !strcmp(command[0], "sudo"));
    assert(wait_seconds > 0 && wait_seconds <= 600 &&
           run_seconds > 0 && run_seconds <= 600);
    assert(!strcmp(lock, "/run/hamn-deployment.lock") ||
           !strcmp(lock, "/run/hamn-retirement.lock"));
    unsigned char random[16];
    char token[33], wait[16], limit[16], total[16];
    arc4random_buf(random, sizeof(random));
    for (int i = 0; i < 16; i++) snprintf(token + 2 * i, 3, "%02x", random[i]);
    snprintf(wait, sizeof(wait), "%u", wait_seconds);
    snprintf(limit, sizeof(limit), "%us", run_seconds);
    snprintf(total, sizeof(total), "%us", wait_seconds + run_seconds + 20);
    const char *wrapped[64] = { "sudo", "timeout", "--kill-after=5s", total,
        "bash", "-c", writer, "--", token, lock, wait, limit };
    size_t count = 12;
    for (size_t i = 1; command[i]; i++) {
        if (count + 1 >= sizeof(wrapped) / sizeof(wrapped[0])) {
            errno = E2BIG; return -1;
        }
        wrapped[count++] = command[i];
    }
    wrapped[count] = NULL;
    int rc = output ? ssh_exec_capture_checked(profile, ip, wrapped, output, capacity, truncated) :
                      ssh_exec(profile, ip, wrapped, 0);
    if (rc != 0 && proc_cancelled()) {
        proc_cleanup_begin();
        (void)operation_phase("fencing-after-cancel");
        const char *publish[] = { "sudo", "bash", "-c", fence, "--", token, NULL };
        const char *barrier[] = { "sudo", "timeout", "--kill-after=5s", total,
            "flock", "--wait", wait, lock, "true", NULL };
        /* Publishing outside the lock also defeats an original writer queued
         * behind us. The barrier alone would not establish FIFO ordering. */
        if (ssh_exec_bounded(profile, ip, publish, 30000) != 0 ||
            ssh_exec_bounded(profile, ip, barrier, (wait_seconds + 10) * 1000) != 0) {
            cleanup_pending = 1;
            logerr("cannot fence and settle cancelled guest mutation; preserving VM");
        }
        proc_cleanup_end();
    }
    return rc;
}
