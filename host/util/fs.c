#include "util/fs.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int fs_mkdirs(const char *path, mode_t mode)
{
    char buf[PATH_MAX];
    size_t len = strlen(path);

    if (len == 0 || len >= sizeof(buf)) {
        errno = EINVAL;
        return -1;
    }
    memcpy(buf, path, len + 1);

    for (char *p = buf + 1; *p; p++) {
        if (*p != '/')
            continue;
        *p = '\0';
        if (mkdir(buf, mode) != 0) {
            if (errno != EEXIST)
                return -1;
            struct stat st;
            if (stat(buf, &st) != 0 || !S_ISDIR(st.st_mode)) {
                errno = ENOTDIR;
                return -1;
            }
        }
        *p = '/';
    }
    if (mkdir(buf, mode) != 0) {
        if (errno != EEXIST)
            return -1;
        struct stat st;
        if (stat(buf, &st) != 0 || !S_ISDIR(st.st_mode)) {
            errno = ENOTDIR;
            return -1;
        }
    }
    return 0;
}

int fs_unlink_if_exists(const char *path)
{
    return unlink(path) == 0 || errno == ENOENT ? 0 : -1;
}

static int full_write(int fd, const char *data, size_t len)
{
    while (len > 0) {
        ssize_t n = write(fd, data, len);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        data += n;
        len -= (size_t)n;
    }
    return 0;
}

#ifdef HAMN_TEST
static int test_parent_fsync_error;

void fs_test_fail_parent_fsync_once(int error)
{
    test_parent_fsync_error = error;
}
#endif

static int atomic_parent_fault(const char *path, const char *stage)
{
    const char *fault_path = getenv("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH");
    const char *fault_stage = getenv("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE");
    if (!fault_path || !fault_stage || strcmp(fault_path, path) != 0 ||
        strcmp(fault_stage, stage) != 0)
        return 0;
    errno = EIO;
    return 1;
}

static int open_parent_dir(const char *path, char *name, size_t name_capacity)
{
    char dir[PATH_MAX];
    size_t len = strlen(path);
    if (len == 0) {
        errno = EINVAL;
        return -1;
    }
    if (len >= sizeof(dir)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(dir, path, len + 1);

    char *slash = strrchr(dir, '/');
    const char *basename = path;
    if (!slash) {
        snprintf(dir, sizeof(dir), ".");
    } else if (slash == dir) {
        slash[1] = '\0';
        basename = path + 1;
    } else {
        basename = path + (slash - dir) + 1;
        *slash = '\0';
    }
    if (!*basename) {
        errno = EINVAL;
        return -1;
    }
    int name_length = snprintf(name, name_capacity, "%s", basename);
    if (name_length < 0 || (size_t)name_length >= name_capacity) {
        errno = ENAMETOOLONG;
        return -1;
    }

    if (atomic_parent_fault(path, "open"))
        return -1;
    int dfd = open(dir, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_DIRECTORY);
    if (dfd < 0)
        return -1;

    struct stat status;
    if (atomic_parent_fault(path, "fstat") || fstat(dfd, &status) != 0)
        goto fail;
    if (!S_ISDIR(status.st_mode)) {
        errno = ENOTDIR;
        goto fail;
    }
    return dfd;

fail: {
    int saved = errno;
    (void)close(dfd);
    errno = saved;
    return -1;
}
}

int fs_write_file_atomic(const char *path, const char *data, size_t len,
                         mode_t mode)
{
    char name[PATH_MAX], tmp[PATH_MAX];
    int directory_fd = open_parent_dir(path, name, sizeof(name));
    if (directory_fd < 0)
        return -1;

    int fd = -1;
    int temporary_exists = 0;
    for (int attempt = 0; attempt < 8; attempt++) {
        uint32_t nonce = arc4random();
        int n = snprintf(tmp, sizeof(tmp), "%s.tmp.%ld.%08x", name,
                         (long)getpid(), nonce);
        if (n < 0 || n >= (int)sizeof(tmp)) {
            errno = ENAMETOOLONG;
            goto out;
        }
        fd = openat(directory_fd, tmp, O_WRONLY | O_CREAT | O_EXCL |
                    O_CLOEXEC | O_NOFOLLOW, mode);
        if (fd >= 0) {
            temporary_exists = 1;
            break;
        }
        if (errno != EEXIST)
            goto out;
    }
    if (fd < 0)
        goto out;

    if (fchmod(fd, mode) != 0 || full_write(fd, data, len) != 0)
        goto out;
    if (fsync(fd) != 0)
        goto out;
    if (close(fd) != 0) {
        fd = -1;
        goto out;
    }
    fd = -1;
#ifdef HAMN_TEST
    const char *fail_before_rename =
        getenv("HAMN_TEST_FS_FAIL_BEFORE_RENAME");
    if (fail_before_rename && strcmp(fail_before_rename, "1") == 0) {
        errno = EIO;
        goto out;
    }
#endif
    if (renameat(directory_fd, tmp, directory_fd, name) != 0)
        goto out;
    temporary_exists = 0;
    if (atomic_parent_fault(path, "fsync"))
        goto out;
#ifdef HAMN_TEST
    if (test_parent_fsync_error != 0) {
        errno = test_parent_fsync_error;
        test_parent_fsync_error = 0;
        goto out;
    }
#endif
    if (fsync(directory_fd) != 0)
        goto out;
    if (atomic_parent_fault(path, "close")) {
        int close_rc = close(directory_fd);
        directory_fd = -1;
        if (close_rc != 0)
            return -1;
        errno = EIO;
        return -1;
    }
    if (close(directory_fd) != 0) {
        directory_fd = -1;
        return -1;
    }
    return 0;

out: {
    int saved = errno;
    if (fd >= 0)
        (void)close(fd);
    if (temporary_exists)
        (void)unlinkat(directory_fd, tmp, 0);
    (void)close(directory_fd);
    errno = saved;
    return -1;
}
}
