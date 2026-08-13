#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

#include "core/lifecycle.h"

static int write_text(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    size_t length = strlen(text);
    size_t offset = 0;
    while (offset < length) {
        ssize_t written = write(fd, text + offset, length - offset);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0) {
            close(fd);
            return -1;
        }
        offset += (size_t)written;
    }
    return close(fd);
}

static int wait_for_release(const char *path)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    char byte;
    ssize_t length;
    do {
        length = read(fd, &byte, sizeof(byte));
    } while (length < 0 && errno == EINTR);
    close(fd);
    return length == 1 ? 0 : -1;
}

static int hold_in_child(const char *profile_name, const char *ready_path,
                         const char *release_path, const char *done_path)
{
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0)
        return 1;
    pid_t child = fork();
    if (child < 0) {
        vm_lifecycle_lock_release(&lock);
        return 1;
    }
    if (child == 0) {
        char ready[64];
        int length = snprintf(ready, sizeof(ready), "%d\n", getpid());
        if (length <= 0 || length >= (int)sizeof(ready) ||
            write_text(ready_path, ready) != 0 ||
            wait_for_release(release_path) != 0) {
            vm_lifecycle_lock_release(&lock);
            _exit(1);
        }
        vm_lifecycle_lock_release(&lock);
        if (write_text(done_path, "done\n") != 0)
            _exit(1);
        _exit(0);
    }

    /* Only the child's inherited descriptor must keep the flock held. */
    vm_lifecycle_lock_release(&lock);
    return 0;
}

static int acquire_once(const char *profile_name, const char *event_path)
{
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0)
        return 1;
    int rc = write_text(event_path, "acquired\n") == 0 ? 0 : 1;
    vm_lifecycle_lock_release(&lock);
    return rc;
}

int main(int argc, char **argv)
{
    if (argc == 6 && strcmp(argv[1], "hold-child") == 0)
        return hold_in_child(argv[2], argv[3], argv[4], argv[5]);
    if (argc == 4 && strcmp(argv[1], "acquire") == 0)
        return acquire_once(argv[2], argv[3]);
    fprintf(stderr,
            "usage: %s hold-child PROFILE READY RELEASE DONE | "
            "acquire PROFILE EVENT\n",
            argv[0]);
    return 2;
}
