#include "core/purge.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <removefile.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>

#include "core/log.h"

#define PURGE_PREFIX ".purge-"
#define PURGE_SUFFIX_HEX 16

#ifdef HAMN_TEST
static int interrupt_after_rename;
void purge_test_interrupt_after_rename(void)
{
    interrupt_after_rename = 1;
}
static int interrupted(void)
{
    int fired = interrupt_after_rename;
    interrupt_after_rename = 0;
    return fired;
}
#else
static int interrupted(void) { return 0; }
#endif

/* Whether name is .purge-<profile name>-<16 lowercase hex digits>; the
 * profile name goes to profile. */
static int purged_name(const char *name, char profile[PROFILE_NAME_CAP])
{
    size_t length = strlen(name), prefix = strlen(PURGE_PREFIX);
    if (length <= prefix + 1 + PURGE_SUFFIX_HEX ||
        strncmp(name, PURGE_PREFIX, prefix) != 0)
        return 0;
    const char *suffix = name + length - PURGE_SUFFIX_HEX;
    if (suffix[-1] != '-' ||
        strspn(suffix, "0123456789abcdef") != PURGE_SUFFIX_HEX)
        return 0;
    size_t profile_length = length - prefix - 1 - PURGE_SUFFIX_HEX;
    if (profile_length >= PROFILE_NAME_CAP)
        return 0;
    memcpy(profile, name + prefix, profile_length);
    profile[profile_length] = '\0';
    return profile_name_valid(profile);
}

/* Whether a process holds the lifecycle lock of profile, or that cannot be
 * told. Opens no lock file that does not exist, and holds none. */
static int lifecycle_held(int home, const char *profile)
{
    char path[PROFILE_NAME_CAP + 16];
    snprintf(path, sizeof(path), ".locks/%s.lock", profile);
    int fd = openat(home, path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0)
        return errno != ENOENT;
    int held = flock(fd, LOCK_SH | LOCK_NB) != 0;
    close(fd);
    return held;
}

/* Remove the tree name in the directory home. A tree that another removal
 * finished first counts as removed. */
static int remove_tree(int home, const char *name)
{
    if (removefileat(home, name, NULL, REMOVEFILE_RECURSIVE) == 0)
        return 0;
    int saved = errno;
    struct stat status;
    if (fstatat(home, name, &status, AT_SYMLINK_NOFOLLOW) != 0 &&
        errno == ENOENT)
        return 0;
    errno = saved;
    return -1;
}

enum purge_result purge_retire(const struct profile *profile)
{
    char root[PROFILE_PATH_CAP];
    int home = hamn_home_open(root);
    if (home < 0) {
        logerr("cannot open ~/.hamn to delete profile %s: %s; nothing was "
               "deleted", profile->name, strerror(errno));
        return PURGE_UNCHANGED;
    }
    unsigned char random[PURGE_SUFFIX_HEX / 2];
    arc4random_buf(random, sizeof(random));
    char hidden[sizeof(PURGE_PREFIX) + PROFILE_NAME_CAP + 1 + PURGE_SUFFIX_HEX];
    int length = snprintf(hidden, sizeof(hidden), PURGE_PREFIX "%s-",
                          profile->name);
    for (size_t i = 0; i < sizeof(random); i++)
        length += snprintf(hidden + length, sizeof(hidden) - (size_t)length,
                           "%02x", random[i]);
    if (renameatx_np(home, profile->name, home, hidden, RENAME_EXCL) != 0) {
        logerr("cannot move profile %s aside to delete it: %s; nothing was "
               "deleted", profile->name, strerror(errno));
        close(home);
        return PURGE_UNCHANGED;
    }
    enum purge_result result = PURGE_HIDDEN;
    if (fsync(home) != 0)
        logerr("cannot make the move of profile %s to %s/%s durable: %s; vm "
               "list removes it", profile->name, root, hidden,
               strerror(errno));
    else if (interrupted())
        logerr("interrupted after moving profile %s to %s/%s", profile->name,
               root, hidden);
    else if (remove_tree(home, hidden) != 0)
        logerr("cannot remove %s/%s, the deleted profile %s: %s; vm list "
               "removes it", root, hidden, profile->name, strerror(errno));
    else
        result = PURGE_REMOVED;
    close(home);
    return result;
}

void purge_sweep(void)
{
    char root[PROFILE_PATH_CAP];
    int home = hamn_home_open(root);
    if (home < 0)
        return;
    DIR *directory = fdopendir(home);
    if (!directory) {
        close(home);
        return;
    }
    int synced = 0;
    struct dirent *entry;
    while ((entry = readdir(directory))) {
        char profile[PROFILE_NAME_CAP];
        struct stat status;
        if (!purged_name(entry->d_name, profile) ||
            fstatat(dirfd(directory), entry->d_name, &status,
                    AT_SYMLINK_NOFOLLOW) != 0 ||
            !S_ISDIR(status.st_mode) || status.st_uid != geteuid())
            continue;
        /* The purge that renamed it holds this lock until it has removed
         * it, and a second removal would make that purge fail. */
        if (lifecycle_held(dirfd(directory), profile))
            continue;
        /* The purge that renamed it can have stopped before its sync. */
        if (!synced && fsync(dirfd(directory)) != 0) {
            logmsg("cannot sync %s: %s; what interrupted purges left stays "
                   "until the next try", root, strerror(errno));
            break;
        }
        synced = 1;
        if (remove_tree(dirfd(directory), entry->d_name) == 0)
            logmsg("removed %s/%s, left by an interrupted vm delete --purge",
                   root, entry->d_name);
        else
            logmsg("cannot remove %s/%s, left by an interrupted vm delete "
                   "--purge: %s", root, entry->d_name, strerror(errno));
    }
    closedir(directory);
}
