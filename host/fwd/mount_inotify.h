#ifndef HAMN_FWD_MOUNT_INOTIFY_H
#define HAMN_FWD_MOUNT_INOTIFY_H

#include "core/profile.h"

/* Start or revoke the per-profile experimental FSEvents bridge. */
int mount_inotify_start(const struct profile *profile);
int mount_inotify_revoke(const struct profile *profile);

/* Internal daemon command; not part of Hamn's public CLI. */
int cmd_mount_inotify_watch(int argc, char **argv);

#endif
