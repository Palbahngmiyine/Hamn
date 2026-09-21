#include "image/disk.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "core/log.h"
#include "image/raw_cache.h"

/* A live profile mutation lock is held by the lifecycle caller. No existing
 * inode is replaced. New disks become visible only after extraction/clone,
 * growth and file fsync have succeeded in a private sibling directory. */
int disk_prepare(const struct profile *p, const char *cache_img)
{
    char disk[1024];
    if (!profile_path(p, "disk.img", disk, sizeof(disk)))
        return -1;
    off_t want = (off_t)p->disk_gib << 30;
    int fd = open(disk, O_RDWR | O_CLOEXEC | O_NOFOLLOW);
    struct stat st;
    if (fd >= 0) {
        int rc = -1;
        if (fstat(fd, &st) == 0 && S_ISREG(st.st_mode) &&
            st.st_uid == geteuid() && st.st_nlink == 1 &&
            !(st.st_mode & 022)) {
            rc = st.st_size < want ? ftruncate(fd, want) : 0;
            if (rc == 0 && st.st_size < want) rc = fsync(fd);
        } else {
            errno = EPERM;
        }
        int saved = errno;
        close(fd);
        errno = saved;
        return rc;
    }
    if (errno != ENOENT)
        return -1;
    /* An existing dangling symlink must never be mistaken for a missing disk. */
    if (lstat(disk, &st) == 0 || errno != ENOENT) {
        errno = EEXIST;
        return -1;
    }
    int parent = open(p->dir, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (parent < 0)
        return -1;
    if (fstat(parent, &st) != 0 || st.st_uid != geteuid() || (st.st_mode & 077)) {
        close(parent);
        errno = EPERM;
        return -1;
    }
    char stage[1100], output[1200];
    int n = snprintf(stage, sizeof(stage), "%s/.disk-stage.XXXXXX", p->dir);
    if (n < 0 || (size_t)n >= sizeof(stage) || !mkdtemp(stage)) {
        close(parent);
        return -1;
    }
    snprintf(output, sizeof(output), "%s/disk.img", stage);
    logmsg("preparing disk image (%u GiB) ...", p->disk_gib);
    int rc = -1;
    if (raw_cache_clone(cache_img, output) != 0 || chmod(output, 0600) != 0)
        goto out;
    fd = open(output, O_RDWR | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0 || fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) ||
        (st.st_size < want && ftruncate(fd, want) != 0) || fsync(fd) != 0)
        goto out;
    close(fd);
    fd = -1;
    if (renamex_np(output, disk, RENAME_EXCL) != 0 || fsync(parent) != 0)
        goto out;
    rc = 0;
out: {
    int saved = errno;
    if (fd >= 0) close(fd);
    unlink(output);
    rmdir(stage);
    close(parent);
    errno = saved;
    return rc;
}
}
