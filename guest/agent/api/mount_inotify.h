#ifndef HAMND_MOUNT_INOTIFY_H
#define HAMND_MOUNT_INOTIFY_H

#include <time.h>

/* Refresh one existing regular file below a writable virtiofs tag. */
int mount_inotify_touch(const char *tag, const char *relative,
                        time_t seconds, long nanoseconds);

#endif
