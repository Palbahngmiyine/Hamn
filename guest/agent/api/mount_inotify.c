#include "api/mount_inotify.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <mntent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define MOUNT_INOTIFY_MAX_TAG_INDEX 15

static const char *mount_table_path(void)
{
#ifdef HAMN_TEST
    const char *path = getenv("HAMND_MOUNT_INOTIFY_MOUNTS_FILE");
    if (path && path[0])
        return path;
#endif
    return "/proc/mounts";
}

static int tag_valid(const char *tag)
{
    if (!tag)
        return 0;
    if (strcmp(tag, "home") == 0)
        return 1;
    if (strncmp(tag, "mount", 5) != 0 || !tag[5])
        return 0;
    char *end = NULL;
    errno = 0;
    long index = strtol(tag + 5, &end, 10);
    return errno == 0 && end && *end == '\0' && index >= 0 &&
        index <= MOUNT_INOTIFY_MAX_TAG_INDEX;
}

static int component_valid(const char *component, size_t length)
{
    if (length == 0 || length > NAME_MAX ||
        (length == 1 && component[0] == '.') ||
        (length == 2 && component[0] == '.' && component[1] == '.'))
        return 0;
    for (size_t index = 0; index < length; index++) {
        unsigned char value = (unsigned char)component[index];
        if (value == '\0' || value < 0x20)
            return 0;
    }
    return 1;
}

static int relative_path_valid(const char *relative)
{
    if (!relative || !relative[0] || relative[0] == '/' ||
        strlen(relative) >= PATH_MAX)
        return 0;
    const char *part = relative;
    while (*part) {
        const char *end = strchr(part, '/');
        size_t length = end ? (size_t)(end - part) : strlen(part);
        if (!component_valid(part, length))
            return 0;
        if (!end)
            return 1;
        part = end + 1;
    }
    return 0;
}

static int tag_mountpoint(const char *tag, char output[PATH_MAX])
{
    FILE *mounts = setmntent(mount_table_path(), "r");
    if (!mounts)
        return -1;
    int matches = 0;
    struct mntent *entry;
    while ((entry = getmntent(mounts)) != NULL) {
        if (strcmp(entry->mnt_type, "virtiofs") != 0 ||
            strcmp(entry->mnt_fsname, tag) != 0)
            continue;
        if (++matches != 1 || entry->mnt_dir[0] != '/' ||
            strlen(entry->mnt_dir) >= PATH_MAX) {
            endmntent(mounts);
            errno = EINVAL;
            return -1;
        }
        snprintf(output, PATH_MAX, "%s", entry->mnt_dir);
    }
    endmntent(mounts);
    if (matches != 1) {
        errno = ENOENT;
        return -1;
    }
    return 0;
}

static int open_existing_regular(const char *mountpoint, const char *relative)
{
    int directory = open(mountpoint, O_RDONLY | O_DIRECTORY | O_CLOEXEC |
                         O_NOFOLLOW);
    if (directory < 0)
        return -1;
    const char *part = relative;
    for (;;) {
        const char *end = strchr(part, '/');
        size_t length = end ? (size_t)(end - part) : strlen(part);
        char component[NAME_MAX + 1];
        if (!component_valid(part, length)) {
            close(directory);
            errno = EINVAL;
            return -1;
        }
        memcpy(component, part, length);
        component[length] = '\0';
        int final = end == NULL;
        int next = openat(directory, component,
                          final ? O_WRONLY | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW :
                          O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
        int saved = errno;
        close(directory);
        if (next < 0) {
            errno = saved;
            return -1;
        }
        if (final) {
            struct stat status;
            if (fstat(next, &status) != 0) {
                saved = errno;
                close(next);
                errno = saved;
                return -1;
            }
            if (!S_ISREG(status.st_mode)) {
                close(next);
                errno = EINVAL;
                return -1;
            }
            return next;
        }
        directory = next;
        part = end + 1;
    }
}

int mount_inotify_touch(const char *tag, const char *relative,
                        time_t seconds, long nanoseconds)
{
    if (!tag_valid(tag) || !relative_path_valid(relative) || seconds < 0 ||
        nanoseconds < 0 || nanoseconds >= 1000000000L) {
        errno = EINVAL;
        return -1;
    }
    char mountpoint[PATH_MAX];
    if (tag_mountpoint(tag, mountpoint) != 0)
        return -1;
    int file = open_existing_regular(mountpoint, relative);
    if (file < 0)
        return -1;
    struct timespec timestamps[2] = {
        { .tv_sec = 0, .tv_nsec = UTIME_OMIT },
        { .tv_sec = seconds, .tv_nsec = nanoseconds },
    };
    int rc = futimens(file, timestamps);
    int saved = errno;
    close(file);
    errno = saved;
    return rc;
}
