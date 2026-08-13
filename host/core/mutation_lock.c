#include "core/mutation_lock.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>

#include "util/fs.h"

int profile_mutation_lock(const struct profile *profile)
{
    if (!profile || !profile->name[0]) {
        errno = EINVAL;
        return -1;
    }
    char root[PATH_MAX];
    if (!hamn_home(root, sizeof(root)) || fs_mkdirs(root, 0700) != 0)
        return -1;
    char path[PATH_MAX];
    int length = snprintf(path, sizeof(path), "%s/.%s-mutation.lock",
                          root, profile->name);
    if (length < 0 || length >= (int)sizeof(path)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    int fd = open(path, O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (fd < 0)
        return -1;
    if (fchmod(fd, 0600) != 0 || flock(fd, LOCK_EX | LOCK_NB) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    return fd;
}

void profile_mutation_unlock(int fd)
{
    if (fd < 0)
        return;
    (void)flock(fd, LOCK_UN);
    (void)close(fd);
}
