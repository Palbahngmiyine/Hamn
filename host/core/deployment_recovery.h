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
 * Exit 0: no backup remains, either because none existed or because the single
 * ready backup was rolled back and removed. Nonzero, with a reason on stderr:
 * the root is unsafe, more than one entry exists, the entry is not an owned
 * 32-hex directory, its phase is not ready (an incomplete backup is never
 * restored), clearing systemd's start-limit state failed (before any restore),
 * or the rollback failed or left its backup behind (the helper retains the
 * backup for another attempt).
 */
#define DEPLOYMENT_RECOVERY_SCRIPT \
    "set -eu; shopt -s nullglob dotglob; root=$1; helper=$2; " \
    "test -e \"$root\" || test -L \"$root\" || exit 0; " \
    "test -d \"$root\" && test ! -L \"$root\" || " \
    "{ echo \"unsafe deployment backup root $root\" >&2; exit 1; }; " \
    "set -- \"$root\"/*; test $# -ne 0 || exit 0; " \
    "test $# -eq 1 || " \
    "{ echo \"multiple deployment backups; inspect $root\" >&2; exit 1; }; " \
    "entry=$1; token=${entry##*/}; " \
    "[[ $token =~ ^[0-9a-f]{32}$ ]] && test -d \"$entry\" && " \
    "test ! -L \"$entry\" && test -O \"$entry\" || " \
    "{ echo \"invalid deployment backup; inspect $entry\" >&2; exit 1; }; " \
    "phase=; if test -f \"$entry/phase\" && test ! -L \"$entry/phase\"; " \
    "then phase=$(head -c 32 \"$entry/phase\"); fi; " \
    "test \"$phase\" = ready || " \
    "{ echo \"incomplete deployment backup; inspect $entry\" >&2; exit 1; }; " \
    /* A retry must not inherit the interrupted attempt's start limit. */ \
    "systemctl reset-failed docker.service docker.socket containerd.service " \
    "hamnd.service hamn-host-dns.service; " \
    "bash \"$helper\" rollback \"$token\"; " \
    "test ! -e \"$entry\" || " \
    "{ echo 'deployment recovery did not remove its backup' >&2; exit 1; }"

#endif
