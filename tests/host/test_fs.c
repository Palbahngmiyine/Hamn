#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "util/fs.h"

static void expect_file(const char *path, const char *expected, size_t len)
{
    char buf[4096];
    int fd = open(path, O_RDONLY);
    assert(fd >= 0);
    assert(read(fd, buf, sizeof(buf)) == (ssize_t)len);
    assert(close(fd) == 0);
    assert(memcmp(buf, expected, len) == 0);
}

static pid_t start_writer(int gate, const char *path, const char *data)
{
    pid_t pid = fork();
    if (pid != 0)
        return pid;
    char byte;
    _exit(read(gate, &byte, 1) != 1 ||
          fs_write_file_atomic(path, data, 4096, 0600) != 0);
}

int main(void)
{
    char dir[] = "/tmp/hamn-fs.XXXXXX";
    assert(mkdtemp(dir));
    char state[256], victim[256], stale[256], removable[256];
    assert(snprintf(state, sizeof(state), "%s/state", dir) > 0);
    assert(snprintf(victim, sizeof(victim), "%s/victim", dir) > 0);
    assert(snprintf(stale, sizeof(stale), "%s.tmp", state) > 0);
    assert(snprintf(removable, sizeof(removable), "%s/removable", dir) > 0);

    assert(fs_write_file_atomic(victim, "safe", 4, 0600) == 0);
    assert(symlink(victim, stale) == 0);
    assert(fs_write_file_atomic(state, "new", 3, 0600) == 0);
    expect_file(victim, "safe", 4);

    static char first[4096], second[4096];
    memset(first, 'A', sizeof(first));
    memset(second, 'B', sizeof(second));
    int gate[2];
    assert(pipe(gate) == 0);
    pid_t writers[] = { start_writer(gate[0], state, first),
                        start_writer(gate[0], state, second) };
    assert(writers[0] > 0 && writers[1] > 0);
    assert(write(gate[1], "xx", 2) == 2);
    assert(close(gate[0]) == 0 && close(gate[1]) == 0);
    for (size_t i = 0; i < 2; i++) {
        int status;
        assert(waitpid(writers[i], &status, 0) == writers[i]);
        assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    }
    char selected;
    int fd = open(state, O_RDONLY);
    assert(fd >= 0 && read(fd, &selected, 1) == 1 && close(fd) == 0);
    expect_file(state, selected == 'A' ? first : second, sizeof(first));

    assert(setenv("HAMN_TEST_FS_FAIL_BEFORE_RENAME", "1", 1) == 0);
    errno = 0;
    assert(fs_write_file_atomic(state, "blocked", 7, 0600) == -1 &&
           errno == EIO);
    assert(unsetenv("HAMN_TEST_FS_FAIL_BEFORE_RENAME") == 0);
    expect_file(state, selected == 'A' ? first : second, sizeof(first));

    fs_test_fail_parent_fsync_once(EIO);
    errno = 0;
    assert(fs_write_file_atomic(state, "fsync", 5, 0600) == -1 &&
           errno == EIO);
    expect_file(state, "fsync", 5);
    assert(fs_write_file_atomic(removable, "x", 1, 0600) == 0);
    assert(fs_unlink_if_exists(removable) == 0);
    assert(fs_unlink_if_exists(removable) == 0);
    assert(mkdir(removable, 0700) == 0);
    errno = 0;
    assert(fs_unlink_if_exists(removable) == -1 && errno != ENOENT);
    assert(rmdir(removable) == 0);
    assert(unlink(stale) == 0 && unlink(state) == 0 && unlink(victim) == 0 &&
           rmdir(dir) == 0);
    puts("PASS: safe atomic file replacement");
    return 0;
}
