#ifndef HAMN_GUEST_FILESYSTEM_H
#define HAMN_GUEST_FILESYSTEM_H

#include <stddef.h>

#include "cjson/cJSON.h"
#include "core/profile.h"

/*
 * Read-only check of the guest root filesystem, sent from the host so that
 * guest images already installed need nothing new:
 *
 *     bash -c GUEST_FILESYSTEM_PROBE_SCRIPT -- MOUNTINFO DEVICES EXT4
 *
 * MOUNTINFO is /proc/self/mountinfo, DEVICES /sys/dev/block and EXT4
 * /sys/fs/ext4; a test passes fixture paths. Only bash builtins run: on a
 * disk the guest cannot read, starting any other program fails, and neither
 * /proc nor /sys is read from that disk. Nothing is written and no root
 * privilege is needed. The last mount at / is the root filesystem.
 *
 * Exit 0 and one line, plus a second line for ext4:
 *
 *     root <major:minor> <type> <mount options> <superblock options>
 *     errors <count> <first error time> <last error time>
 *
 * The ext4 values are the superblock's error record (the kernel's sysfs
 * errors_count, first_error_time and last_error_time; times in seconds since
 * the epoch, 0 when unset). e2fsck clears the record only after a complete
 * check, so a count above zero means errors happened since the last one.
 * Exit 3 when the root mount cannot be found, 4 when the ext4 record cannot
 * be read, with the reason on stderr.
 */
#define GUEST_FILESYSTEM_PROBE_SCRIPT \
    "set -u; mountinfo=$1; devices=$2; ext4=$3; found=; " \
    "while read -r -a field; do " \
    "test \"${field[4]-}\" = / || continue; " \
    /* Optional fields end at the lone '-' before type, source, options. */ \
    "i=6; while test \"$i\" -lt \"${#field[@]}\" && " \
    "test \"${field[i]}\" != -; do i=$((i + 1)); done; " \
    "test $((i + 3)) -lt \"${#field[@]}\" || continue; " \
    "found=1; device=${field[2]}; type=${field[i + 1]}; " \
    "mount_options=${field[5]}; super_options=${field[i + 3]}; " \
    "done < \"$mountinfo\" || exit 3; " \
    "test -n \"$found\" || { echo \"no root mount in $mountinfo\" >&2; exit 3; }; " \
    "printf 'root %s %s %s %s\\n' \"$device\" \"$type\" \"$mount_options\" " \
    "\"$super_options\"; " \
    "test \"$type\" = ext4 || exit 0; " \
    /* The block device's name, as ext4 names its sysfs directory. */ \
    "name=$(cd -P \"$devices/$device\" && printf '%s' \"${PWD##*/}\") && " \
    "test -n \"$name\" || exit 4; " \
    "read -r count < \"$ext4/$name/errors_count\" && " \
    "read -r first < \"$ext4/$name/first_error_time\" && " \
    "read -r last < \"$ext4/$name/last_error_time\" || exit 4; " \
    "printf 'errors %s %s %s\\n' \"$count\" \"$first\" \"$last\""

/* What the check found. Every state except UNKNOWN is evidence about the
 * guest's own disk; UNKNOWN says only that the check could not tell. */
enum guest_filesystem_state {
    GUEST_FILESYSTEM_UNKNOWN = 0,
    GUEST_FILESYSTEM_HEALTHY,
    /* ext4 recorded errors since its last complete check. */
    GUEST_FILESYSTEM_ERRORS_RECORDED,
    /* The guest failed to read its own disk: a guest program reported EIO,
     * typically sshd that cannot execute the login shell. */
    GUEST_FILESYSTEM_UNREADABLE,
    /* The root filesystem is mounted read-only. */
    GUEST_FILESYSTEM_READ_ONLY,
};

#define GUEST_FILESYSTEM_DETAIL_CAP 256

struct guest_filesystem_check {
    enum guest_filesystem_state state;
    /* One line for a message: the evidence, or why the check could not
     * tell. NUL-terminated, cut to fit. */
    char detail[GUEST_FILESYSTEM_DETAIL_CAP];
};

/* Classify what the probe printed. rc is its exit status as the SSH client
 * reported it: 0..254 come from the guest, 255 is the client's own failure,
 * negative a local failure. output is stdout and stderr together; line
 * ends may follow the last line. timed_out and cancelled say that the run
 * ended early; the output is then partial and the result UNKNOWN.
 * Otherwise, with rc 0..254, a line ending in ": Input/output error" is
 * UNREADABLE: sshd, the login shell or the probe failed to read the guest's
 * disk. Without one, only rc 0 with output in exactly the documented form is
 * evidence (ERRORS_RECORDED before READ_ONLY before HEALTHY); anything else
 * is UNKNOWN with the reason. Guest text in detail has its control
 * characters replaced. Pure: no I/O. */
void guest_filesystem_classify(int rc, const char *output, int timed_out,
                               int cancelled,
                               struct guest_filesystem_check *check);

/* 1 for the states that a start cannot repair: errors recorded,
 * unreadable, read-only. */
int guest_filesystem_damaged(const struct guest_filesystem_check *check);

/* Run the probe through the profile's SSH master with a deadline of
 * timeout_ms and classify it. Never fails: a check that cannot run is
 * UNKNOWN with the reason. */
void guest_filesystem_probe(const struct profile *profile, const char *ip,
                            unsigned timeout_ms,
                            struct guest_filesystem_check *check);

/*
 * The profile's record of a damaged guest filesystem, guest-filesystem.json:
 * a private regular file written atomically. It names the disk.img it was
 * observed on by device and inode, so a record does not describe a disk
 * that replaced that file. The caller holds the profile mutation lock.
 *
 * record: write the damaged check (state must be damaged). Returns 0, or -1
 * with errno; the earlier record is then unchanged.
 * forget: remove the record. Returns 0 when none remains, or -1 with errno.
 */
int guest_filesystem_record(const struct profile *profile,
                            const struct guest_filesystem_check *check);
int guest_filesystem_forget(const struct profile *profile);

/* The record as `vm status` reports it: JSON null when there is no record
 * or it describes a disk that is not the present disk.img; otherwise an
 * object {"state", "detail", "observedAt"} where state is "errorsRecorded",
 * "unreadable" or "readOnly" and observedAt the time of the check in
 * seconds since the epoch. A record that cannot be validated, or a disk
 * that cannot be examined, is {"state": "unknown", "detail": <why>,
 * "observedAt": null}. Returns NULL only when memory runs out. Read-only. */
cJSON *guest_filesystem_snapshot(const struct profile *profile);

#endif
