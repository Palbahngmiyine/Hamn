#ifndef HAMN_DEPLOYMENT_RECOVERY_H
#define HAMN_DEPLOYMENT_RECOVERY_H

/*
 * Guest-side recovery of an interrupted deployment transaction, run as root
 * under the guest deployment lock:
 *
 *     bash -c DEPLOYMENT_RECOVERY_SCRIPT -- ROOT HELPER
 *
 * ROOT is the transaction directory (/var/lib/hamn/deployment-transactions)
 * and HELPER the image's guest-deployment-transaction script. The script is
 * sent from the host because images that are already installed have no
 * recovery action of their own.
 *
 * The single entry's phase file decides; an incomplete backup is never
 * restored:
 *   - exactly the bytes "ready\n": the backup is complete. systemd's start
 *     limits are cleared, then the helper restores and removes the backup.
 *   - missing, or a regular file holding anything else (empty, or
 *     zero-filled after a crash): the backup was never published, or commit
 *     or rollback had already retired it. Its begin changes nothing outside
 *     the entry and the host changes the guest only after begin publishes
 *     ready durably, so there is nothing to roll back. The entry is removed,
 *     the removal is made durable, and one line on stderr names it. Images
 *     before 0.1.1 published ready without fsync, so after a crash their
 *     empty phase can accompany applied changes; that backup cannot be
 *     restored either, and the start that follows redeploys over them.
 *   - a symlink or other non-regular file: refused.
 *   - a phase that cannot be read: refused. Its backup may be ready, so it
 *     is neither restored nor discarded.
 *
 * Exit 0: no backup remains, either because none existed, because a backup
 * that was not ready was discarded, or because the single ready backup was
 * rolled back and removed. Nonzero, with a reason on stderr: the root is
 * unsafe, more than one entry exists, the entry is not an owned 32-hex
 * directory, its phase is unsafe or unreadable, removing a discarded entry
 * failed, clearing systemd's start-limit state failed (before any restore),
 * or the rollback failed or left its backup behind (the helper retains the
 * backup for another attempt).
 */
#define DEPLOYMENT_RECOVERY_SCRIPT \
    "set -euo pipefail; shopt -s nullglob dotglob; root=$1; helper=$2; " \
    "test -e \"$root\" || test -L \"$root\" || exit 0; " \
    "test -d \"$root\" && test ! -L \"$root\" || " \
    "{ echo \"unsafe deployment backup root $root\" >&2; exit 1; }; " \
    "set -- \"$root\"/*; test $# -ne 0 || exit 0; " \
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
    "state='not ready'; test \"$bytes\" != 72656164790a || state=ready; fi; " \
    "if test \"$state\" != ready; then " \
    "rm -rf -- \"$entry\"; sync -- \"$root\"; " \
    "echo \"discarded deployment backup $token (phase $state): it was never " \
    "published or was already retired, so it is never restored\" >&2; " \
    "exit 0; fi; " \
    /* A retry must not inherit the interrupted attempt's start limit. */ \
    "systemctl reset-failed docker.service docker.socket containerd.service " \
    "hamnd.service hamn-host-dns.service; " \
    "bash \"$helper\" rollback \"$token\"; " \
    "test ! -e \"$entry\" || " \
    "{ echo 'deployment recovery did not remove its backup' >&2; exit 1; }"

#endif
