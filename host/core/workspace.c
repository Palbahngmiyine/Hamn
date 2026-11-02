#include "core/workspace.h"

#include <CommonCrypto/CommonDigest.h>
#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/param.h>
#include <sys/stat.h>
#include <unistd.h>

#include "core/log.h"
#include "util/fs.h"

_Static_assert(PATH_MAX >= MAXPATHLEN,
               "fcntl(F_GETPATH) writes up to MAXPATHLEN bytes");

static int open_directory_at(int dir, const char *name)
{
    return openat(dir, name, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
}

static void workspace_profile_name(const char *path,
                                   char name[PROFILE_NAME_CAP])
{
    unsigned char digest[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256(path, (CC_LONG)strlen(path), digest);
    snprintf(name, PROFILE_NAME_CAP, "ws-%02x%02x%02x%02x%02x%02x",
             digest[0], digest[1], digest[2], digest[3], digest[4],
             digest[5]);
}

int workspace_identify(const char *path, struct workspace *workspace)
{
    memset(workspace, 0, sizeof(*workspace));
    if (!path || path[0] != '/') {
        logerr("the workspace must be an absolute path");
        return -1;
    }
    int fd = open_directory_at(AT_FDCWD, path);
    if (fd < 0) {
        if (errno == ENOENT)
            logerr("workspace %s does not exist", path);
        else if (errno == ENOTDIR)
            logerr("workspace %s is not a directory", path);
        else
            logerr("cannot open workspace %s: %s", path, strerror(errno));
        return -1;
    }
    struct stat status;
    int rc = -1;
    if (fstat(fd, &status) != 0 ||
        fcntl(fd, F_GETPATH, workspace->path) != 0) {
        logerr("cannot read workspace %s: %s", path, strerror(errno));
    } else if (status.st_uid != geteuid()) {
        logerr("workspace %s is not owned by this user", path);
    } else {
        workspace->device = status.st_dev;
        workspace->inode = status.st_ino;
        workspace_profile_name(workspace->path, workspace->profile);
        rc = 0;
    }
    close(fd);
    return rc;
}

/* Reads the regular file name, relative to dir, into text, NUL-terminated:
 * at most capacity - 1 bytes. With private, the file must be this user's
 * and is not reached through a symbolic link. Returns its length, or -1
 * with errno: EFBIG for a longer file, EINVAL for one that is not a regular
 * file or holds a NUL byte, EPERM for another user's. */
static ssize_t read_small_file(int dir, const char *name, int private,
                               char *text, size_t capacity)
{
    int fd = openat(dir, name, O_RDONLY | O_CLOEXEC | O_NONBLOCK |
                    (private ? O_NOFOLLOW : 0));
    if (fd < 0)
        return -1;
    int error = 0;
    struct stat status;
    if (fstat(fd, &status) != 0)
        error = errno;
    else if (!S_ISREG(status.st_mode))
        error = EINVAL;
    else if (private && status.st_uid != geteuid())
        error = EPERM;
    size_t used = 0;
    while (!error && used < capacity) {
        ssize_t count = read(fd, text + used, capacity - used);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0)
            error = errno;
        else if (count == 0)
            break;
        else
            used += (size_t)count;
    }
    close(fd);
    if (!error && used == capacity)
        error = EFBIG;
    if (!error && memchr(text, '\0', used))
        error = EINVAL;
    if (error) {
        errno = error;
        return -1;
    }
    text[used] = '\0';
    return (ssize_t)used;
}

/* The first line of a git pointer file: its text without trailing line
 * breaks, as git reads .git and commondir. */
static int read_git_pointer(int dir, const char *name, char *text,
                            size_t capacity)
{
    ssize_t length = read_small_file(dir, name, 0, text, capacity);
    if (length < 0)
        return -1;
    while (length > 0 &&
           (text[length - 1] == '\n' || text[length - 1] == '\r'))
        text[--length] = '\0';
    return 0;
}

/* Whether name, relative to dir, is a directory (directory set) or a
 * regular file, following symbolic links. */
static int has_entry(int dir, const char *name, int directory)
{
    struct stat status;
    return fstatat(dir, name, &status, 0) == 0 &&
           (directory ? S_ISDIR(status.st_mode) : S_ISREG(status.st_mode));
}

/* The path of the common git directory common, as F_GETPATH gives it, once
 * the pair is laid out as git requires of a repository (is_git_directory):
 * a HEAD file in the git directory, objects and refs directories in the
 * common one. A .git file is input from the workspace: one that names the
 * home directory or ~/.ssh must not get it shared with the VM. */
static int git_directory_path(int gitdir, int common, const char *path,
                              char git[PATH_MAX],
                              char reason[WORKSPACE_REASON_CAP])
{
    if (!has_entry(gitdir, "HEAD", 0) || !has_entry(common, "objects", 1) ||
        !has_entry(common, "refs", 1)) {
        snprintf(reason, WORKSPACE_REASON_CAP,
                 "the git directory of %s is not one: it lacks a HEAD file "
                 "or objects and refs directories", path);
        return -1;
    }
    if (fcntl(common, F_GETPATH, git) == 0)
        return 0;
    snprintf(reason, WORKSPACE_REASON_CAP,
             "cannot read the path of the git directory of %s: %s", path,
             strerror(errno));
    return -1;
}

/* The common git directory of a linked worktree or a submodule, whose .git
 * is a file that names its git directory. */
static int linked_git_directory(int workspace, const char *path,
                                char git[PATH_MAX],
                                char reason[WORKSPACE_REASON_CAP])
{
    static const char prefix[] = "gitdir: ";
    char text[PATH_MAX + sizeof(prefix)];
    if (read_git_pointer(workspace, ".git", text, sizeof(text)) != 0) {
        snprintf(reason, WORKSPACE_REASON_CAP, "cannot read %s/.git: %s",
                 path, strerror(errno));
        return -1;
    }
    const char *named = text + sizeof(prefix) - 1;
    if (strncmp(text, prefix, sizeof(prefix) - 1) != 0 || !named[0]) {
        snprintf(reason, WORKSPACE_REASON_CAP,
                 "%s/.git is a file that does not name a git directory "
                 "with \"gitdir: <path>\"", path);
        return -1;
    }
    int gitdir = open_directory_at(workspace, named);
    if (gitdir < 0) {
        snprintf(reason, WORKSPACE_REASON_CAP,
                 "cannot open the git directory %s that %s/.git names: %s",
                 named, path, strerror(errno));
        return -1;
    }
    /* Without a commondir file the git directory is its own common one. */
    int common = gitdir;
    char named_common[PATH_MAX];
    if (read_git_pointer(gitdir, "commondir", named_common,
                         sizeof(named_common)) == 0) {
        common = open_directory_at(gitdir, named_common);
        if (common < 0)
            snprintf(reason, WORKSPACE_REASON_CAP,
                     "cannot open the common git directory %s that %s/"
                     "commondir names: %s", named_common, named,
                     strerror(errno));
    } else if (errno != ENOENT) {
        snprintf(reason, WORKSPACE_REASON_CAP, "cannot read %s/commondir: %s",
                 named, strerror(errno));
        common = -1;
    }
    int rc = common < 0 ? -1 :
        git_directory_path(gitdir, common, path, git, reason);
    if (common >= 0 && common != gitdir)
        close(common);
    close(gitdir);
    return rc;
}

int workspace_git_directory(const char *path, char git[PATH_MAX],
                            char reason[WORKSPACE_REASON_CAP])
{
    git[0] = '\0';
    reason[0] = '\0';
    int workspace = open_directory_at(AT_FDCWD, path);
    if (workspace < 0) {
        snprintf(reason, WORKSPACE_REASON_CAP, "cannot open %s: %s", path,
                 strerror(errno));
        return -1;
    }
    int rc = -1;
    struct stat status;
    if (fstatat(workspace, ".git", &status, 0) != 0) {
        snprintf(reason, WORKSPACE_REASON_CAP, "%s/.git: %s", path,
                 errno == ENOENT ? "there is none" : strerror(errno));
    } else if (S_ISREG(status.st_mode)) {
        rc = linked_git_directory(workspace, path, git, reason);
    } else if (!S_ISDIR(status.st_mode)) {
        snprintf(reason, WORKSPACE_REASON_CAP,
                 "%s/.git is neither a directory nor a file", path);
    } else {
        int fd = open_directory_at(workspace, ".git");
        if (fd < 0) {
            snprintf(reason, WORKSPACE_REASON_CAP, "cannot open %s/.git: %s",
                     path, strerror(errno));
        } else {
            rc = git_directory_path(fd, fd, path, git, reason);
            close(fd);
        }
    }
    if (rc != 0)
        git[0] = '\0';
    close(workspace);
    return rc;
}

static void add_mount(struct profile *profile, const char *path, int writable)
{
    struct profile_mount *mount = &profile->mounts[profile->mount_count++];
    snprintf(mount->location, sizeof(mount->location), "%s", path);
    snprintf(mount->mount_point, sizeof(mount->mount_point), "%s", path);
    mount->writable = writable;
}

void workspace_profile(const struct workspace *workspace, const char *git,
                       struct profile *profile)
{
    profile_defaults(profile);
    snprintf(profile->name, sizeof(profile->name), "%s", workspace->profile);
    profile->mount_home = 0;
    add_mount(profile, workspace->path, 1);
    if (git[0])
        add_mount(profile, git, 0);
}

int workspace_record_format(const struct workspace *workspace,
                            char text[WORKSPACE_RECORD_CAP])
{
    int length = snprintf(text, WORKSPACE_RECORD_CAP, "%llu %llu %s\n",
                          (unsigned long long)workspace->device,
                          (unsigned long long)workspace->inode,
                          workspace->path);
    if (length < 0 || length >= WORKSPACE_RECORD_CAP) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return length;
}

int workspace_record_write(const struct profile *profile,
                           const struct workspace *workspace)
{
    char path[PROFILE_PATH_CAP], text[WORKSPACE_RECORD_CAP];
    int length = workspace_record_format(workspace, text);
    if (length < 0)
        return -1;
    if (!profile_path(profile, WORKSPACE_RECORD_FILE, path, sizeof(path))) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return fs_write_file_atomic(path, text, (size_t)length, 0600);
}

/* Digits only, then one space; *cursor moves past the space. */
static int read_decimal(const char **cursor, unsigned long long *value)
{
    if (!isdigit((unsigned char)**cursor))
        return -1;
    char *end = NULL;
    errno = 0;
    *value = strtoull(*cursor, &end, 10);
    if (errno || *end != ' ')
        return -1;
    *cursor = end + 1;
    return 0;
}

int workspace_record_parse(const char *text, struct workspace *recorded)
{
    memset(recorded, 0, sizeof(*recorded));
    const char *cursor = text;
    unsigned long long device = 0, inode = 0;
    size_t path_length = 0;
    if (read_decimal(&cursor, &device) == 0 &&
        read_decimal(&cursor, &inode) == 0) {
        path_length = strlen(cursor);
        recorded->device = (dev_t)device;
        recorded->inode = (ino_t)inode;
    }
    /* The path, absolute and of one line, ends the record with a newline. */
    if (path_length < 2 || path_length > PATH_MAX || cursor[0] != '/' ||
        memchr(cursor, '\n', path_length - 1) ||
        cursor[path_length - 1] != '\n' ||
        (unsigned long long)recorded->device != device ||
        (unsigned long long)recorded->inode != inode) {
        memset(recorded, 0, sizeof(*recorded));
        return -1;
    }
    memcpy(recorded->path, cursor, path_length - 1);
    recorded->path[path_length - 1] = '\0';
    workspace_profile_name(recorded->path, recorded->profile);
    return 0;
}

int workspace_record_read(const struct profile *profile,
                          struct workspace *recorded)
{
    memset(recorded, 0, sizeof(*recorded));
    char text[WORKSPACE_RECORD_CAP];
    ssize_t length = -1;
    int dir = open_directory_at(AT_FDCWD, profile->dir);
    if (dir >= 0) {
        length = read_small_file(dir, WORKSPACE_RECORD_FILE, 1, text,
                                 sizeof(text));
        int saved = errno;
        close(dir);
        errno = saved;
    }
    if (length < 0) {
        if (errno == ENOENT)
            return 0;
        logerr("cannot read the workspace record %s/%s: %s", profile->dir,
               WORKSPACE_RECORD_FILE, strerror(errno));
        return -1;
    }
    /* read_small_file refuses a NUL byte, so text ends where the file
     * does. */
    if (workspace_record_parse(text, recorded) != 0) {
        logerr("the workspace record %s/%s is not \"<device> <inode> "
               "<path>\" on one line", profile->dir, WORKSPACE_RECORD_FILE);
        return -1;
    }
    return 1;
}

int workspace_same(const struct workspace *a, const struct workspace *b)
{
    return strcmp(a->path, b->path) == 0 && a->device == b->device &&
           a->inode == b->inode;
}

enum workspace_presence workspace_presence(const char *path)
{
    struct stat status;
    if (lstat(path, &status) == 0)
        return WORKSPACE_PRESENT;
    return errno == ENOENT || errno == ENOTDIR ? WORKSPACE_ABSENT :
        WORKSPACE_UNKNOWN;
}
