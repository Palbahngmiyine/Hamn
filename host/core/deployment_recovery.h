#ifndef HAMN_DEPLOYMENT_RECOVERY_H
#define HAMN_DEPLOYMENT_RECOVERY_H

/*
 * Guest-side recovery of an interrupted deployment transaction, run as root
 * under the guest deployment lock:
 *
 *     bash -c DEPLOYMENT_RECOVERY_SCRIPT -- ROOT HELPER RECORD [DIGEST...]
 *
 * ROOT is the transaction directory (/var/lib/hamn/deployment-transactions),
 * HELPER the image's guest-deployment-transaction script, and RECORD a file
 * (/var/lib/hamn/deployment-reapply) whose existence says that a recovery
 * changed the guest runtime and the whole runtime has not been applied
 * again since. Each DIGEST is the SHA-256 of a helper that published ready
 * without fsync (the host passes DEPLOYMENT_UNDURABLE_HELPERS). The script
 * is sent from the host because images that are already installed have no
 * recovery action of their own.
 *
 * Before a discard or a rollback removes the evidence of an interrupted
 * transaction, the script creates RECORD and makes it durable, so the need
 * for a whole reapplication outlives an interruption of this script or of
 * the host. The host removes RECORD only after a transaction that applied
 * the whole runtime committed. An unsafe RECORD (symlink, non-regular) is
 * refused before anything else.
 *
 * The single entry's phase file decides; an incomplete backup is never
 * restored:
 *   - exactly the bytes "ready\n": the backup is complete. systemd's start
 *     limits are cleared, then the helper restores and removes the backup.
 *   - missing: the backup was never published, or commit or rollback had
 *     already retired it. Its begin changes nothing outside the entry and
 *     the host changes the guest only after begin publishes ready durably,
 *     so there is nothing to roll back.
 *   - exactly six zero bytes: a published phase whose removal a crash lost
 *     after its freed block was discarded (observed with the guest root's
 *     `discard` mount option). The phase is removed only after commit or
 *     rollback decided the outcome, or by a failed begin, so there is
 *     nothing to roll back either.
 *   In both cases the entry is removed, the removal is made durable, and one
 *   line on stderr names it. Both conclusions rest on a durable begin: when
 *   HELPER's digest is a DIGEST, a crash can also lose an in-place phase
 *   after the host changed the guest, so both states are refused and kept
 *   as ambiguous instead (a missing helper is refused too).
 *   - any other bytes (empty, truncated or foreign): no helper writes them
 *     since 0.1.1, so they cannot show that the guest was left unchanged.
 *     Images before 0.1.1 published ready without fsync, so an empty phase
 *     there can accompany applied changes. Refused and kept; the message
 *     names the entry and how to keep the current guest runtime.
 *   - a symlink or other non-regular file: refused.
 *   - a phase that cannot be read: refused. Its backup may be ready, so it
 *     is neither restored nor discarded.
 *
 * Exit 0: no backup remains, either because none existed, because a backup
 * that was not ready was discarded, or because the single ready backup was
 * rolled back and removed. It prints nothing only when none existed and
 * RECORD does not exist: a discard or a rollback prints one line on stderr
 * that names the entry and what was done, and a RECORD left by an earlier
 * recovery is reported in one line, because in each case the guest runtime
 * may no longer be the deployment the host marker records. Nonzero, with a
 * reason on stderr: RECORD is unsafe or cannot be made durable (nothing is
 * removed then), the root is unsafe, more than one entry exists, the entry is
 * not an owned 32-hex directory, its phase is unsafe, unreadable, damaged or
 * ambiguous, the helper cannot be identified, removing a discarded entry
 * failed, clearing systemd's start-limit state failed (before any restore),
 * or the rollback failed or left its backup behind (the helper retains the
 * backup for another attempt).
 */
#define DEPLOYMENT_RECOVERY_SCRIPT \
    "set -euo pipefail; shopt -s nullglob dotglob; root=$1; helper=$2; " \
    "record=$3; shift 3; undurable=\" $* \"; " \
    "if test -L \"$record\" || { test -e \"$record\" && test ! -f \"$record\"; }; " \
    "then echo \"unsafe deployment reapply record; inspect $record\" >&2; " \
    "exit 1; fi; " \
    "nothing() { test ! -e \"$record\" || echo \"the whole guest runtime is " \
    "still to be applied again after an earlier deployment recovery\" >&2; " \
    "exit 0; }; " \
    "remember() { : > \"$record\" && sync -- \"$record\" \"${record%/*}\" || " \
    "{ echo \"cannot record that the whole guest runtime must be applied " \
    "again; nothing was recovered\" >&2; exit 1; }; }; " \
    "test -e \"$root\" || test -L \"$root\" || nothing; " \
    "test -d \"$root\" && test ! -L \"$root\" || " \
    "{ echo \"unsafe deployment backup root $root\" >&2; exit 1; }; " \
    "set -- \"$root\"/*; test $# -ne 0 || nothing; " \
    "test $# -eq 1 || " \
    "{ echo \"multiple deployment backups; inspect $root\" >&2; exit 1; }; " \
    "entry=$1; token=${entry##*/}; phase=$entry/phase; " \
    "[[ $token =~ ^[0-9a-f]{32}$ ]] && test -d \"$entry\" && " \
    "test ! -L \"$entry\" && test -O \"$entry\" || " \
    "{ echo \"invalid deployment backup; inspect $entry\" >&2; exit 1; }; " \
    "if test -L \"$phase\" || { test -e \"$phase\" && test ! -f \"$phase\"; }; " \
    "then echo \"unsafe deployment backup phase; inspect $entry\" >&2; " \
    "exit 1; fi; " \
    /* Bytes are compared because command substitution would drop NULs. */ \
    "state=missing; if test -f \"$phase\"; then " \
    "bytes=$(head -c 7 \"$phase\" | od -An -tx1 | tr -d ' \\n') || " \
    "{ echo \"cannot read deployment backup phase; inspect $entry\" >&2; " \
    "exit 1; }; " \
    "case $bytes in 72656164790a) state=ready ;; " \
    "000000000000) state=zero-filled ;; " \
    "*) echo \"damaged deployment backup phase; neither restored nor " \
    "discarded: inspect $entry, and remove it to keep the current guest " \
    "runtime\" >&2; exit 1 ;; esac; fi; " \
    "if test \"$state\" != ready; then " \
    "digest=$(sha256sum < \"$helper\") || { echo \"cannot identify the " \
    "deployment helper $helper; inspect $entry\" >&2; exit 1; }; " \
    "case $undurable in *\" ${digest%% *} \"*) echo \"ambiguous deployment " \
    "backup (phase $state) from a helper that published without fsync " \
    "(Hamn before 0.1.1): the guest may be partly configured, so it is " \
    "neither restored nor discarded: inspect $entry, and remove it to keep " \
    "the current guest runtime\" >&2; exit 1 ;; esac; " \
    "remember; rm -rf -- \"$entry\"; sync -- \"$root\"; " \
    "echo \"discarded deployment backup $token (phase $state): it was never " \
    "published or was already retired, so it is never restored\" >&2; " \
    "exit 0; fi; " \
    "remember; " \
    /* A retry must not inherit the interrupted attempt's start limit. */ \
    "systemctl reset-failed docker.service docker.socket containerd.service " \
    "hamnd.service hamn-host-dns.service; " \
    "bash \"$helper\" rollback \"$token\"; " \
    "test ! -e \"$entry\" || " \
    "{ echo 'deployment recovery did not remove its backup' >&2; exit 1; }; " \
    "echo \"rolled back deployment backup $token: the guest runtime is the " \
    "one from before its transaction\" >&2"

/*
 * SHA-256 of the guest-deployment-transaction helpers that the Hamn 0.0.1
 * and 0.1.0 guest images install, the DIGEST arguments above. They wrote
 * phase in place without fsync before returning; every later helper
 * publishes ready durably. Derived with
 *     git show v0.0.1:guest/scripts/guest-deployment-transaction.sh |
 *         shasum -a 256
 * and likewise for v0.1.0; signed images install that file unchanged.
 */
#define DEPLOYMENT_UNDURABLE_HELPERS \
    "1627767c1297df2c84e54c07adedef697b8766769e90b76a7627d21557e56540", \
    "912bfb92669467768ae7eecae07ef2f405d4f6d0308eb81431595b101dd9c901"

#endif
