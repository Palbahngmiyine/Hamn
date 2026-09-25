/* The profile lifecycle lock is held by every process that inherits its
 * descriptor: a forked supervisor keeps start/stop/delete serialized after the
 * process that acquired the lock has released its own copy and exited. */
#include <assert.h>
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#include "core/lifecycle.h"

static void write_byte(int fd, char byte)
{
    ssize_t written;
    do {
        written = write(fd, &byte, 1);
    } while (written < 0 && errno == EINTR);
    if (written != 1)
        _exit(90);
}

/* Returns the next byte, 0 at end of file, or -1 when `timeout_ms` passes. */
static int read_byte(int fd, int timeout_ms)
{
    struct pollfd ready = { .fd = fd, .events = POLLIN };
    int count;
    do {
        count = poll(&ready, 1, timeout_ms);
    } while (count < 0 && errno == EINTR);
    assert(count >= 0);
    if (count == 0)
        return -1;
    char byte;
    ssize_t length;
    do {
        length = read(fd, &byte, 1);
    } while (length < 0 && errno == EINTR);
    assert(length >= 0);
    return length == 0 ? 0 : (unsigned char)byte;
}

static void wait_success(pid_t pid)
{
    int status;
    pid_t reaped;
    do {
        reaped = waitpid(pid, &status, 0);
    } while (reaped < 0 && errno == EINTR);
    assert(reaped == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

int main(void)
{
    char home[] = "/tmp/hamn-lifecycle-lock-XXXXXX";
    assert(mkdtemp(home) && setenv("HOME", home, 1) == 0);
    int ready[2], release[2], acquired[2];
    assert(pipe(ready) == 0 && pipe(release) == 0 && pipe(acquired) == 0);

    /* The acquirer forks a holder with the inherited lock, releases its own
     * descriptor and exits. */
    pid_t acquirer = fork();
    assert(acquirer >= 0);
    if (acquirer == 0) {
        struct vm_lifecycle_lock lock;
        if (vm_lifecycle_lock_acquire("profile", &lock) != 0)
            _exit(91);
        pid_t holder = fork();
        if (holder < 0)
            _exit(92);
        if (holder == 0) {
            write_byte(ready[1], 'r');
            if (read_byte(release[0], 10000) != 'x')
                _exit(93);
            vm_lifecycle_lock_release(&lock);
            _exit(0);
        }
        vm_lifecycle_lock_release(&lock);
        _exit(0);
    }
    wait_success(acquirer);
    close(ready[1]);
    close(release[0]);
    assert(read_byte(ready[0], 10000) == 'r');

    pid_t waiter = fork();
    assert(waiter >= 0);
    if (waiter == 0) {
        struct vm_lifecycle_lock lock;
        if (vm_lifecycle_lock_acquire("profile", &lock) != 0)
            _exit(94);
        write_byte(acquired[1], 'a');
        vm_lifecycle_lock_release(&lock);
        _exit(0);
    }
    close(acquired[1]);
    /* A bounded negative check: the waiter must still be blocked. */
    assert(read_byte(acquired[0], 500) == -1);
    struct vm_lifecycle_lock other;
    assert(vm_lifecycle_lock_acquire("other", &other) == 0);
    vm_lifecycle_lock_release(&other);

    write_byte(release[1], 'x');
    assert(read_byte(acquired[0], 10000) == 'a');
    assert(read_byte(acquired[0], 10000) == 0);
    wait_success(waiter);
    /* The holder is a grandchild; its exit closed `ready`'s last writer. */
    assert(read_byte(ready[0], 10000) == 0);

    struct vm_lifecycle_lock invalid;
    assert(vm_lifecycle_lock_acquire("../escape", &invalid) == -1 && invalid.fd == -1);
    assert(vm_lifecycle_lock_acquire("", &invalid) == -1);

    char command[sizeof(home) + 16];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", home);
    assert(system(command) == 0);
    puts("PASS: a lifecycle lock is held by inherited descriptors until the last holder releases it");
    return 0;
}
