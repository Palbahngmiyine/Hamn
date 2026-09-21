#include "image/raw_cache.h"

#include <CommonCrypto/CommonDigest.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/clonefile.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "image/qcow2.h"

#define EXTRACTOR_VERSION 1
#define MAX_VIRTUAL_BYTES (UINT64_C(8) << 30)

#ifdef HAMN_TEST
static const char *failure_point;
static int failure_error;
void raw_cache_test_fail(const char *point, int error)
{
    failure_point = point;
    failure_error = error;
}
static int fail_at(const char *point)
{
    if (failure_point && strcmp(failure_point, point) == 0) {
        failure_point = NULL;
        errno = failure_error;
        return 1;
    }
    return 0;
}
#else
static int fail_at(const char *point) { (void)point; return 0; }
#endif

static int join(char *output, size_t cap, const char *directory, const char *name)
{
    int n = snprintf(output, cap, "%s/%s", directory, name);
    if (n < 0 || (size_t)n >= cap) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

static int owned_file(int fd, mode_t mode, struct stat *st)
{
    if (fstat(fd, st) != 0)
        return -1;
    if (!S_ISREG(st->st_mode) || st->st_uid != geteuid() ||
        st->st_nlink != 1 || (st->st_mode & 07777) != mode) {
        errno = EPERM;
        return -1;
    }
    return 0;
}

static int directory_open(const char *path, int private)
{
    int fd = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    struct stat st;
    if (fd < 0)
        return -1;
    if (fstat(fd, &st) != 0 || st.st_uid != geteuid() ||
        (st.st_mode & (private ? 077 : 022)) != 0) {
        close(fd);
        errno = EPERM;
        return -1;
    }
    return fd;
}

/* Hash from the open descriptor, rejecting concurrent changes rather than
 * accidentally binding a digest to a different file at the same pathname. */
static int hash_file(int fd, char output[65])
{
    struct stat before, after;
    if (fstat(fd, &before) != 0 || lseek(fd, 0, SEEK_SET) < 0)
        return -1;
    CC_SHA256_CTX context;
    CC_SHA256_Init(&context);
    unsigned char bytes[65536], digest[CC_SHA256_DIGEST_LENGTH];
    for (;;) {
        ssize_t count = read(fd, bytes, sizeof(bytes));
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0)
            return -1;
        if (count == 0)
            break;
        CC_SHA256_Update(&context, bytes, (CC_LONG)count);
    }
    CC_SHA256_Final(digest, &context);
    if (fstat(fd, &after) != 0 || before.st_size != after.st_size ||
        before.st_mtimespec.tv_sec != after.st_mtimespec.tv_sec ||
        before.st_mtimespec.tv_nsec != after.st_mtimespec.tv_nsec ||
        before.st_ctimespec.tv_sec != after.st_ctimespec.tv_sec ||
        before.st_ctimespec.tv_nsec != after.st_ctimespec.tv_nsec) {
        errno = EIO;
        return -1;
    }
    for (size_t i = 0; i < sizeof(digest); i++)
        snprintf(output + i * 2, 3, "%02x", digest[i]);
    return 0;
}

static int lock_digest(int fd)
{
    struct timespec started, now, interval = { .tv_nsec = 10000000 };
    if (clock_gettime(CLOCK_MONOTONIC, &started) != 0)
        return -1;
    while (flock(fd, LOCK_EX | LOCK_NB) != 0) {
        if (errno != EWOULDBLOCK && errno != EINTR)
            return -1;
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
            return -1;
        if (now.tv_sec - started.tv_sec >= 60) {
            errno = ETIMEDOUT;
            return -1;
        }
        nanosleep(&interval, NULL);
    }
    return 0;
}

static int marker_text(char output[256], const char *guest, uint64_t size,
                       const char *raw)
{
    return snprintf(output, 256, "hamn-raw-base %d\n%s\n%" PRIu64 "\n%s\n",
                    EXTRACTOR_VERSION, guest, size, raw);
}

static int validate_bundle(const char *bundle, const char *guest, uint64_t size,
                           int *raw_out)
{
    int dir = directory_open(bundle, 1);
    if (dir < 0)
        return -1;
    int raw = openat(dir, "disk.raw", O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    int marker = openat(dir, "verified", O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    int rc = -1;
    struct stat st;
    char digest[65], expected[256], actual[256];
    if (raw < 0 || marker < 0 || owned_file(raw, 0400, &st) != 0 ||
        st.st_size < 0 || (uint64_t)st.st_size != size ||
        hash_file(raw, digest) != 0 || owned_file(marker, 0400, &st) != 0)
        goto out;
    int length = marker_text(expected, guest, size, digest);
    if (st.st_size != length || pread(marker, actual, sizeof(actual), 0) != length ||
        memcmp(actual, expected, (size_t)length) != 0) {
        errno = EINVAL;
        goto out;
    }
    *raw_out = raw;
    raw = -1;
    rc = 0;
out:
    if (raw >= 0) close(raw);
    if (marker >= 0) close(marker);
    close(dir);
    return rc;
}

/* A crash may leave the digest-local stage. Only the two owned regular files
 * produced by this module can be removed, while its digest lock is held. */
static int discard_stage(const char *stage)
{
    int fd = directory_open(stage, 1);
    if (fd < 0) return errno == ENOENT ? 0 : -1;
    DIR *directory = fdopendir(fd);
    if (!directory) { close(fd); return -1; }
    int rc = -1;
    struct dirent *entry;
    for (;;) {
        errno = 0;
        entry = readdir(directory);
        if (!entry) { if (errno) goto out; break; }
        if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, "..")) continue;
        struct stat st;
        if ((strcmp(entry->d_name, "disk.raw") && strcmp(entry->d_name, "verified")) ||
            fstatat(fd, entry->d_name, &st, AT_SYMLINK_NOFOLLOW) != 0 ||
            !S_ISREG(st.st_mode) || st.st_uid != geteuid() || st.st_nlink != 1 ||
            (st.st_mode & 022)) { errno = EPERM; goto out; }
    }
    if ((unlinkat(fd, "disk.raw", 0) != 0 && errno != ENOENT) ||
        (unlinkat(fd, "verified", 0) != 0 && errno != ENOENT)) goto out;
    rc = rmdir(stage);
out: {
    int saved = errno;
    closedir(directory);
    errno = saved;
    return rc;
}
}

static int create_bundle(int source, const char *parent,
                         const char *bundle, const char *guest, uint64_t size)
{
    char stage[PATH_MAX], raw[PATH_MAX], marker[PATH_MAX];
    int n = snprintf(stage, sizeof(stage), "%s.stage", bundle);
    if (n < 0 || (size_t)n >= sizeof(stage)) { errno = ENAMETOOLONG; return -1; }
    if (discard_stage(stage) != 0 || mkdir(stage, 0700) != 0) return -1;
    int rc = -1, fd = -1, dir = -1;
    char *error = NULL, digest[65], text[256];
    raw[0] = marker[0] = '\0';
    if (join(raw, sizeof(raw), stage, "disk.raw") != 0 ||
        join(marker, sizeof(marker), stage, "verified") != 0 ||
        fail_at("extract") || qcow2_extract_fd(source, raw, &error) != 0)
        goto out;
    fd = open(raw, O_RDWR | O_NOFOLLOW | O_CLOEXEC);
    struct stat st;
    if (hash_file(source, digest) != 0 || strcmp(digest, guest) != 0 ||
        fd < 0 || fstat(fd, &st) != 0 || st.st_size < 0 ||
        (uint64_t)st.st_size != size || hash_file(fd, digest) != 0 ||
        fchmod(fd, 0400) != 0 || fail_at("raw-sync") || fsync(fd) != 0)
        goto out;
    close(fd);
    fd = open(marker, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    int length = marker_text(text, guest, size, digest);
    if (fd < 0 || fail_at("marker-write") ||
        write(fd, text, (size_t)length) != length || fchmod(fd, 0400) != 0 ||
        fsync(fd) != 0)
        goto out;
    close(fd);
    fd = -1;
    dir = directory_open(stage, 1);
    if (dir < 0 || fail_at("stage-sync") || fsync(dir) != 0 ||
        fail_at("publish") || renamex_np(stage, bundle, RENAME_EXCL) != 0)
        goto out;
    stage[0] = raw[0] = marker[0] = '\0';
    close(dir);
    dir = directory_open(parent, 0);
    if (dir < 0 || fail_at("parent-sync") || fsync(dir) != 0)
        goto out;
    rc = 0;
out: {
    int saved = errno;
    free(error);
    if (fd >= 0) close(fd);
    if (dir >= 0) close(dir);
    if (*raw) unlink(raw);
    if (*marker) unlink(marker);
    if (*stage) rmdir(stage);
    errno = saved;
    return rc;
}
}

int raw_cache_clone(const char *image, const char *target)
{
    char parent[PATH_MAX], guest[65], actual[65], name[128];
    char bundle[PATH_MAX], lock[PATH_MAX];
    if (!image || !target || strlen(image) >= sizeof(parent)) {
        errno = EINVAL;
        return -1;
    }
    strcpy(parent, image);
    char *slash = strrchr(parent, '/');
    if (!slash || strncmp(slash + 1, "hamn-guest-", 11) != 0 ||
        strlen(slash + 1) != 79 || strcmp(slash + 76, ".img") != 0) {
        errno = EINVAL;
        return -1;
    }
    memcpy(guest, slash + 12, 64);
    guest[64] = '\0';
    for (size_t i = 0; i < 64; i++) {
        if (!((guest[i] >= '0' && guest[i] <= '9') ||
              (guest[i] >= 'a' && guest[i] <= 'f'))) {
            errno = EINVAL;
            return -1;
        }
    }
    *slash = '\0';
    int parent_fd = directory_open(parent, 0);
    if (parent_fd < 0)
        return -1;
    int source = -1, guard = -1, raw = -1, rc = -1;
    source = open(image, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    struct stat st;
    unsigned char header[32];
    if (source < 0 || fstat(source, &st) != 0 || !S_ISREG(st.st_mode) ||
        st.st_uid != geteuid() || st.st_nlink != 1 || (st.st_mode & 022) ||
        st.st_size <= 0 || (uint64_t)st.st_size >= (UINT64_C(2) << 30) ||
        hash_file(source, actual) != 0 || strcmp(actual, guest) != 0 ||
        pread(source, header, sizeof(header), 0) != sizeof(header)) {
        errno = EINVAL;
        goto out;
    }
    uint64_t size = 0;
    for (size_t i = 24; i < 32; i++) size = (size << 8) | header[i];
    if (!size || size > MAX_VIRTUAL_BYTES || memcmp(header, "QFI\xfb", 4) != 0) {
        errno = EINVAL;
        goto out;
    }
    snprintf(name, sizeof(name), "raw-v%d-%s", EXTRACTOR_VERSION, guest);
    if (join(bundle, sizeof(bundle), parent, name) != 0)
        goto out;
    snprintf(name, sizeof(name), ".raw-v%d-%s.lock", EXTRACTOR_VERSION, guest);
    if (join(lock, sizeof(lock), parent, name) != 0)
        goto out;
    guard = open(lock, O_RDWR | O_CREAT | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (guard < 0 || owned_file(guard, 0600, &st) != 0 || lock_digest(guard) != 0)
        goto out;
    if (lstat(bundle, &st) != 0) {
        if (errno != ENOENT || create_bundle(source, parent, bundle, guest, size) != 0)
            goto out;
    }
    if (validate_bundle(bundle, guest, size, &raw) != 0)
        goto out;
    if (!fail_at("clone") && fclonefileat(raw, AT_FDCWD, target, CLONE_NOOWNERCOPY) == 0) {
        rc = 0;
    } else if (errno == EXDEV || errno == ENOTSUP || errno == EOPNOTSUPP) {
        /* Reserve a nonexistent target even when the filesystem reported its
         * clone limitation before checking destination existence. */
        int output = open(target, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
        if (output < 0) goto out;
        close(output);
        char *error = NULL;
        rc = qcow2_extract_fd(source, target, &error);
        free(error);
        if (rc == 0 && (hash_file(source, actual) != 0 || strcmp(actual, guest) != 0 ||
                        chmod(target, 0400) != 0)) {
            int saved = errno;
            unlink(target);
            errno = saved;
            rc = -1;
        }
    }
out: {
    int saved = errno;
    if (raw >= 0) close(raw);
    if (source >= 0) close(source);
    if (guard >= 0) close(guard);
    close(parent_fd);
    errno = saved;
    return rc;
}
}
