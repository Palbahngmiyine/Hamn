#include <CommonCrypto/CommonDigest.h>
#include <assert.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "core/profile.h"
#include "image/disk.h"
#include "image/qcow2.h"
#include "image/raw_cache.h"

#define VIRTUAL_SIZE (16 * 1024 * 1024)
static char counter[1024];
static int pause_ready = -1, pause_wait = -1;

/* Link-time observation counts real extractor calls across child processes. */
int test_extract(int source, const char *target, char **error)
{
    int fd = open(counter, O_WRONLY | O_CREAT | O_APPEND, 0600);
    assert(fd >= 0 && write(fd, "x", 1) == 1);
    close(fd);
    int rc = qcow2_extract_fd(source, target, error);
    if (pause_ready >= 0) {
        assert(write(pause_ready, "r", 1) == 1);
        char signal;
        (void)read(pause_wait, &signal, 1);
    }
    return rc;
}

void logmsg(const char *format, ...) { (void)format; }
const char *profile_path(const struct profile *p, const char *name,
                         char *output, size_t cap)
{
    int n = snprintf(output, cap, "%s/%s", p->dir, name);
    return n >= 0 && (size_t)n < cap ? output : NULL;
}

static void be(unsigned char *where, uint64_t value, size_t length)
{
    while (length) { where[--length] = value & 255; value >>= 8; }
}

static void digest(const unsigned char *bytes, size_t size, char out[65])
{
    unsigned char hash[32];
    CC_SHA256(bytes, (CC_LONG)size, hash);
    for (size_t i = 0; i < 32; i++) snprintf(out + i * 2, 3, "%02x", hash[i]);
}

static void fixture(const char *root, const char *label, char image[1024],
                    char bundle[1024])
{
    char directory[1024], hash[65];
    snprintf(directory, sizeof(directory), "%s/%s", root, label);
    assert(mkdir(directory, 0700) == 0);
    unsigned char *bytes = calloc(4, 65536);
    assert(bytes);
    be(bytes, 0x514649fb, 4); be(bytes + 4, 2, 4);
    be(bytes + 20, 16, 4); be(bytes + 24, VIRTUAL_SIZE, 8);
    be(bytes + 36, 1, 4); be(bytes + 40, 65536, 8);
    be(bytes + 65536, 131072, 8); be(bytes + 131072, 196608, 8);
    for (size_t i = 196608; i < 262144; i++) bytes[i] = (unsigned char)i;
    digest(bytes, 262144, hash);
    snprintf(image, 1024, "%s/hamn-guest-%s.img", directory, hash);
    snprintf(bundle, 1024, "%s/raw-v1-%s", directory, hash);
    int fd = open(image, O_WRONLY | O_CREAT | O_EXCL, 0600);
    assert(fd >= 0 && write(fd, bytes, 262144) == 262144);
    close(fd); free(bytes);
}

static void content(const char *path)
{
    int fd = open(path, O_RDONLY);
    struct stat st;
    unsigned char bytes[4096];
    assert(fd >= 0 && fstat(fd, &st) == 0 && st.st_size >= VIRTUAL_SIZE);
    assert(pread(fd, bytes, sizeof(bytes), 0) == sizeof(bytes));
    for (size_t i = 0; i < sizeof(bytes); i++) assert(bytes[i] == (unsigned char)i);
    assert(pread(fd, bytes, sizeof(bytes), 65536) == sizeof(bytes));
    for (size_t i = 0; i < sizeof(bytes); i++) assert(bytes[i] == 0);
    close(fd);
}

static off_t length(const char *path)
{
    struct stat st;
    assert(stat(path, &st) == 0);
    return st.st_size;
}

static void no_stages(const char *directory)
{
    DIR *dir = opendir(directory);
    assert(dir);
    struct dirent *entry;
    while ((entry = readdir(dir))) {
        size_t size = strlen(entry->d_name);
        assert(size < 6 || strcmp(entry->d_name + size - 6, ".stage") != 0);
        assert(strncmp(entry->d_name, ".disk-stage.", 12) != 0);
    }
    closedir(dir);
}

int main(int argc, char **argv)
{
    assert(argc == 2);
    alarm(30); /* Bound failed child/barrier synchronization as well as locks. */
    const char *root = argv[1];
    char image[1024], bundle[1024], a[1200], b[1200], raw[1200], marker[1200];
    snprintf(counter, sizeof(counter), "%s/extractions", root);
    fixture(root, "normal", image, bundle);
    snprintf(a, sizeof(a), "%s/a.raw", root);
    snprintf(b, sizeof(b), "%s/b.raw", root);
    assert(raw_cache_clone(image, a) == 0);
    assert(raw_cache_clone(image, b) == 0 && length(counter) == 1);
    content(a); content(b);
    snprintf(raw, sizeof(raw), "%s/disk.raw", bundle);
    snprintf(marker, sizeof(marker), "%s/verified", bundle);
    content(raw);
    assert(chmod(a, 0600) == 0);
    int fd = open(a, O_RDWR);
    assert(fd >= 0);
    /* Reproducible varying offsets include allocated and initially sparse pages. */
    uint32_t seed = 0x1badb002;
    for (int i = 0; i < 128; i++) {
        seed = seed * 1664525 + 1013904223;
        off_t offset = seed % VIRTUAL_SIZE;
        unsigned char old_a, old_b, old_raw, changed;
        int other = open(b, O_RDONLY), base = open(raw, O_RDONLY);
        assert(other >= 0 && base >= 0);
        assert(pread(fd, &old_a, 1, offset) == 1);
        assert(pread(other, &old_b, 1, offset) == 1);
        assert(pread(base, &old_raw, 1, offset) == 1);
        changed = old_a ^ 255;
        assert(pwrite(fd, &changed, 1, offset) == 1);
        assert(pread(other, &changed, 1, offset) == 1 && changed == old_b);
        assert(pread(base, &changed, 1, offset) == 1 && changed == old_raw);
        close(other); close(base);
    }
    close(fd); unlink(a); unlink(b);
    const int unsupported[] = { EXDEV, ENOTSUP, EOPNOTSUPP };
    for (size_t i = 0; i < sizeof(unsupported) / sizeof(unsupported[0]); i++) {
        raw_cache_test_fail("clone", unsupported[i]);
        assert(raw_cache_clone(image, a) == 0);
        content(a); unlink(a);
    }
    int existing = open(a, O_WRONLY | O_CREAT | O_EXCL, 0600);
    assert(existing >= 0 && write(existing, "preserve", 8) == 8); close(existing);
    raw_cache_test_fail("clone", EXDEV);
    assert(raw_cache_clone(image, a) == -1 && length(a) == 8); unlink(a);
    const int fatal[] = { EPERM, EACCES, EIO, ENOSPC };
    for (size_t i = 0; i < sizeof(fatal) / sizeof(fatal[0]); i++) {
        off_t before = length(counter);
        raw_cache_test_fail("clone", fatal[i]);
        assert(raw_cache_clone(image, a) == -1 && errno == fatal[i]);
        assert(access(a, F_OK) != 0 && length(counter) == before);
    }
    /* Permission, marker, digest, hardlink and symlink errors fail closed. */
    assert(chmod(raw, 0600) == 0);
    assert(raw_cache_clone(image, a) == -1);
    assert(chmod(raw, 0400) == 0 && chmod(marker, 0600) == 0);
    assert(raw_cache_clone(image, a) == -1);
    assert(chmod(marker, 0400) == 0 && link(raw, b) == 0);
    assert(raw_cache_clone(image, a) == -1 && unlink(b) == 0);
    assert(chmod(raw, 0600) == 0);
    fd = open(raw, O_WRONLY); assert(fd >= 0 && pwrite(fd, "!", 1, 42) == 1);
    close(fd); assert(chmod(raw, 0400) == 0);
    assert(raw_cache_clone(image, a) == -1);
    assert(unlink(marker) == 0 && symlink(image, marker) == 0);
    assert(raw_cache_clone(image, a) == -1);

    const char *points[] = { "extract", "raw-sync", "marker-write", "stage-sync", "publish", "parent-sync" };
    for (size_t i = 0; i < sizeof(points) / sizeof(points[0]); i++) {
        fixture(root, points[i], image, bundle);
        raw_cache_test_fail(points[i], EIO);
        assert(raw_cache_clone(image, a) == -1 && access(a, F_OK) != 0);
        char directory[1024]; snprintf(directory, sizeof(directory), "%s/%s", root, points[i]);
        no_stages(directory);
        /* A second failure during recovery does not create a visible profile. */
        raw_cache_test_fail("clone", EIO);
        assert(raw_cache_clone(image, a) == -1 && access(a, F_OK) != 0);
        raw_cache_test_fail(NULL, 0);
        assert(raw_cache_clone(image, a) == 0); content(a); unlink(a);
    }

    fixture(root, "concurrent", image, bundle);
    off_t before = length(counter);
    pid_t children[4];
    int barrier[2]; assert(pipe(barrier) == 0);
    for (int i = 0; i < 4; i++) {
        children[i] = fork(); assert(children[i] >= 0);
        if (children[i] == 0) {
            close(barrier[1]); char signal;
            assert(read(barrier[0], &signal, 1) == 1);
            snprintf(a, sizeof(a), "%s/parallel-%d", root, i);
            _exit(raw_cache_clone(image, a) == 0 ? 0 : 1);
        }
    }
    close(barrier[0]); assert(write(barrier[1], "xxxx", 4) == 4); close(barrier[1]);
    for (int i = 0; i < 4; i++) {
        int status; assert(waitpid(children[i], &status, 0) == children[i]);
        assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
        snprintf(a, sizeof(a), "%s/parallel-%d", root, i); content(a);
    }
    assert(length(counter) == before + 1);

    fixture(root, "killed", image, bundle);
    snprintf(a, sizeof(a), "%s/after-kill.raw", root);
    int ready[2], resume[2]; assert(pipe(ready) == 0 && pipe(resume) == 0);
    pid_t killed = fork(); assert(killed >= 0);
    if (killed == 0) {
        close(ready[0]); close(resume[1]);
        pause_ready = ready[1]; pause_wait = resume[0];
        _exit(raw_cache_clone(image, a) == 0 ? 0 : 1);
    }
    close(ready[1]); close(resume[0]);
    char ready_byte; assert(read(ready[0], &ready_byte, 1) == 1);
    assert(kill(killed, SIGKILL) == 0);
    int status; assert(waitpid(killed, &status, 0) == killed && WIFSIGNALED(status));
    close(ready[0]); close(resume[1]);
    assert(raw_cache_clone(image, a) == 0); content(a); unlink(a);
    char killed_dir[1024]; snprintf(killed_dir, sizeof(killed_dir), "%s/killed", root);
    no_stages(killed_dir);

    struct profile p = { .disk_gib = 1 };
    snprintf(p.dir, sizeof(p.dir), "%s/profile", root); assert(mkdir(p.dir, 0700) == 0);
    assert(disk_prepare(&p, image) == 0);
    snprintf(a, sizeof(a), "%s/disk.img", p.dir); content(a);
    struct stat initial, after; assert(stat(a, &initial) == 0 && initial.st_size == (1LL << 30));
    fd = open(a, O_WRONLY); assert(fd >= 0 && pwrite(fd, "saved", 5, 4096) == 5); close(fd);
    assert(disk_prepare(&p, "/invalid/must-not-be-opened") == 0);
    assert(stat(a, &after) == 0 && after.st_ino == initial.st_ino);
    p.disk_gib = 2; assert(disk_prepare(&p, "/invalid") == 0);
    assert(stat(a, &after) == 0 && after.st_ino == initial.st_ino && after.st_size == (2LL << 30));
    fd = open(a, O_RDONLY); char saved[5];
    assert(fd >= 0 && pread(fd, saved, 5, 4096) == 5 && memcmp(saved, "saved", 5) == 0); close(fd);
    assert(unlink(a) == 0 && symlink("missing", a) == 0);
    assert(disk_prepare(&p, image) == -1 && lstat(a, &after) == 0 && S_ISLNK(after.st_mode));
    no_stages(p.dir);
    puts("PASS: raw cache hash/permissions, atomic recovery, real APFS CoW isolation, single-flight and existing disk preservation");
    return 0;
}
