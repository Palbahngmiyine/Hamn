#include "api/cri_status.h"

#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CTR_PATH "/usr/bin/ctr"
#define CRI_OUTPUT_CAP (64 * 1024)
#define CRI_COMMAND_TIMEOUT_MS 250

static int monotonic_milliseconds(uint64_t *milliseconds)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return -1;
    *milliseconds = (uint64_t)now.tv_sec * 1000 +
                    (uint64_t)now.tv_nsec / 1000000;
    return 0;
}

static int set_nonblocking(int fd)
{
    int value = fcntl(fd, F_GETFL);
    return value < 0 || fcntl(fd, F_SETFL, value | O_NONBLOCK) != 0 ? -1 : 0;
}

static int set_close_on_exec(int fd)
{
    int value = fcntl(fd, F_GETFD);
    if (value < 0 || fcntl(fd, F_SETFD, value | FD_CLOEXEC) != 0)
        return -1;
    return 0;
}

static void stop_child(pid_t child)
{
    int result;
    do {
        result = kill(child, SIGKILL);
    } while (result != 0 && errno == EINTR);
    if (result != 0 && errno != ESRCH)
        return;
    while (waitpid(child, NULL, 0) < 0 && errno == EINTR)
        ;
}

static int drain_output(int fd, char *output, size_t *length)
{
    for (;;) {
        ssize_t count = read(fd, output + *length, CRI_OUTPUT_CAP - *length);
        if (count > 0) {
            *length += (size_t)count;
            if (*length == CRI_OUTPUT_CAP)
                return -1;
            continue;
        }
        if (count == 0 || (count < 0 && (errno == EAGAIN ||
                                         errno == EWOULDBLOCK)))
            return 0;
        if (count < 0 && errno == EINTR)
            continue;
        return -1;
    }
}

static int field_contains(const char *field, size_t length, const char *value)
{
    size_t value_length = strlen(value);
    if (value_length > length)
        return 0;
    for (size_t offset = 0; offset <= length - value_length; offset++) {
        if (memcmp(field + offset, value, value_length) == 0)
            return 1;
    }
    return 0;
}

static int field_equals(const char *field, size_t length, const char *value)
{
    return length == strlen(value) && memcmp(field, value, length) == 0;
}

static int cri_line_is_ready(const char *line, size_t length)
{
    const char *first = NULL, *second = NULL, *last = NULL;
    size_t first_length = 0, second_length = 0, last_length = 0;
    const char *cursor = line;
    const char *end = line + length;
    unsigned fields = 0;

    while (cursor < end) {
        while (cursor < end && isspace((unsigned char)*cursor))
            cursor++;
        const char *field = cursor;
        while (cursor < end && !isspace((unsigned char)*cursor))
            cursor++;
        if (field == cursor)
            break;
        size_t field_length = (size_t)(cursor - field);
        if (fields == 0) {
            first = field;
            first_length = field_length;
        } else if (fields == 1) {
            second = field;
            second_length = field_length;
        }
        last = field;
        last_length = field_length;
        fields++;
    }
    return fields >= 2 && field_equals(last, last_length, "ok") &&
           (field_contains(first, first_length, "cri") ||
            field_contains(second, second_length, "cri"));
}

static int cri_output_is_ready(const char *output, size_t length)
{
    size_t start = 0;
    for (size_t cursor = 0; cursor <= length; cursor++) {
        if (cursor == length || output[cursor] == '\n') {
            if (cri_line_is_ready(output + start, cursor - start))
                return 1;
            start = cursor + 1;
        }
    }
    return 0;
}

static int cri_plugin_ready_with_command(const char *ctr_path,
                                         unsigned timeout_ms)
{
    if (!ctr_path || !*ctr_path || timeout_ms == 0)
        return 0;
    int pipefd[2] = { -1, -1 };
    if (pipe(pipefd) != 0 || set_nonblocking(pipefd[0]) != 0 ||
        set_close_on_exec(pipefd[0]) != 0 ||
        set_close_on_exec(pipefd[1]) != 0) {
        if (pipefd[0] >= 0)
            close(pipefd[0]);
        if (pipefd[1] >= 0)
            close(pipefd[1]);
        return 0;
    }
    pid_t child = fork();
    if (child < 0) {
        close(pipefd[0]);
        close(pipefd[1]);
        return 0;
    }
    if (child == 0) {
        int nullfd = open("/dev/null", O_RDWR | O_CLOEXEC);
        if (nullfd < 0 || dup2(nullfd, STDIN_FILENO) < 0 ||
            dup2(pipefd[1], STDOUT_FILENO) < 0 ||
            dup2(nullfd, STDERR_FILENO) < 0)
            _exit(127);
        close(pipefd[0]);
        if (pipefd[1] != STDOUT_FILENO)
            close(pipefd[1]);
        if (nullfd > STDERR_FILENO)
            close(nullfd);
        char *const argv[] = { (char *)ctr_path, "--address",
                               HAMND_CONTAINERD_SOCKET, "plugins", "ls", NULL };
        execv(ctr_path, argv);
        _exit(127);
    }

    close(pipefd[1]);
    uint64_t now;
    if (monotonic_milliseconds(&now) != 0 ||
        now > UINT64_MAX - timeout_ms) {
        close(pipefd[0]);
        stop_child(child);
        return 0;
    }
    uint64_t deadline = now + timeout_ms;
    char output[CRI_OUTPUT_CAP];
    size_t length = 0;
    int status = 0;
    for (;;) {
        if (drain_output(pipefd[0], output, &length) != 0) {
            close(pipefd[0]);
            stop_child(child);
            return 0;
        }
        pid_t waited;
        do {
            waited = waitpid(child, &status, WNOHANG);
        } while (waited < 0 && errno == EINTR);
        if (waited == child)
            break;
        if (waited < 0 || monotonic_milliseconds(&now) != 0 || now >= deadline) {
            close(pipefd[0]);
            stop_child(child);
            return 0;
        }
        uint64_t remaining = deadline - now;
        struct pollfd descriptor = { .fd = pipefd[0], .events = POLLIN };
        int ready;
        do {
            ready = poll(&descriptor, 1,
                         remaining > INT_MAX ? INT_MAX : (int)remaining);
        } while (ready < 0 && errno == EINTR);
        if (ready < 0 || (descriptor.revents & (POLLERR | POLLNVAL))) {
            close(pipefd[0]);
            stop_child(child);
            return 0;
        }
    }
    int read_result = drain_output(pipefd[0], output, &length);
    close(pipefd[0]);
    return read_result == 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0 &&
           cri_output_is_ready(output, length);
}

int cri_plugin_ready(void)
{
    return cri_plugin_ready_with_command(CTR_PATH, CRI_COMMAND_TIMEOUT_MS);
}

#ifdef HAMN_TEST
int cri_plugin_ready_for_test(const char *ctr_path, unsigned timeout_ms)
{
    return cri_plugin_ready_with_command(ctr_path, timeout_ms);
}
#endif
