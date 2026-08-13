#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "api/mount_inotify.h"

static void fail(const char *message)
{
    fprintf(stderr, "FAIL: mountInotify guest safety: %s\n", message);
    exit(1);
}

static void expect_reject(const char *tag, const char *path)
{
    if (mount_inotify_touch(tag, path, 1700000000, 7) == 0)
        fail("unsafe target was accepted");
}

int main(void)
{
    char work[] = "/tmp/hamn-mount-inotify.XXXXXX";
    char *root = mkdtemp(work);
    if (!root)
        fail("cannot create fixture directory");
    char nested[PATH_MAX], regular[PATH_MAX], link[PATH_MAX], mounts[PATH_MAX];
    snprintf(nested, sizeof(nested), "%s/nested", root);
    snprintf(regular, sizeof(regular), "%s/file.txt", nested);
    snprintf(link, sizeof(link), "%s/link.txt", nested);
    snprintf(mounts, sizeof(mounts), "%s/mounts", root);
    if (mkdir(nested, 0700) != 0) {
        rmdir(root);
        fail("cannot create fixture subdirectory");
    }
    int fd = open(regular, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0 || write(fd, "x", 1) != 1 || close(fd) != 0)
        fail("cannot create regular fixture");
    if (symlink("file.txt", link) != 0)
        fail("cannot create symlink fixture");
    FILE *table = fopen(mounts, "w");
    if (!table || fprintf(table, "home %s virtiofs rw 0 0\n", root) < 0 ||
        fclose(table) != 0)
        fail("cannot create mount fixture");
    if (setenv("HAMND_MOUNT_INOTIFY_MOUNTS_FILE", mounts, 1) != 0)
        fail("cannot set mount fixture environment");

    if (mount_inotify_touch("home", "nested/file.txt", 1700000000, 7) != 0)
        fail("regular target was rejected");
    struct stat status;
    if (stat(regular, &status) != 0 || status.st_mtim.tv_sec != 1700000000 ||
        status.st_mtim.tv_nsec != 7)
        fail("regular target timestamp was not refreshed exactly");

    expect_reject("home", "../nested/file.txt");
    expect_reject("home", "/nested/file.txt");
    expect_reject("home", "nested/../file.txt");
    expect_reject("home", "nested/link.txt");
    expect_reject("home", "nested");
    expect_reject("mount16", "nested/file.txt");
    expect_reject("missing", "nested/file.txt");
    if (mount_inotify_touch("home", "nested/file.txt", -1, 0) == 0 ||
        mount_inotify_touch("home", "nested/file.txt", 1700000000,
                            1000000000) == 0)
        fail("invalid timestamp was accepted");

    unlink(link);
    unlink(regular);
    unlink(mounts);
    rmdir(nested);
    rmdir(root);
    printf("PASS: mountInotify guest path and timestamp boundaries are safe\n");
    return 0;
}
