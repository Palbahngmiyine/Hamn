/*
 * Test-only helpers for guest Bash tests, replacing their former Python
 * snippets. Never installed into the guest image.
 *
 *   bind-unix PATH       create a Unix stream socket file at PATH and exit
 *   unix-request PATH    send stdin to PATH, half-close, print the response's
 *                        status line (2 s receive deadline)
 *   agent-status FILE    assert hamnd's /v1/status JSON contract
 *   mtime-ns FILE        print FILE's modification time in nanoseconds
 *
 * Exit status: 0 success, 1 failed assertion or I/O error (reason on
 * stderr), 2 usage error.
 */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>

#include "json/strict_json.h"

#define RESPONSE_MAX_BYTES (1024 * 1024)
#define RECEIVE_DEADLINE_SECONDS 2

static int fail(const char *what)
{
    fprintf(stderr, "guest-test-fixture: %s: %s\n", what, strerror(errno));
    return 1;
}

static int unix_address(const char *path, struct sockaddr_un *address)
{
    memset(address, 0, sizeof(*address));
    address->sun_family = AF_UNIX;
    if (strlen(path) >= sizeof(address->sun_path)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    strcpy(address->sun_path, path);
    return 0;
}

static int bind_unix(const char *path)
{
    struct sockaddr_un address;
    if (unix_address(path, &address) != 0)
        return fail("socket path");
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return fail("socket");
    if (bind(fd, (struct sockaddr *)&address, sizeof(address)) != 0) {
        close(fd);
        return fail("bind");
    }
    close(fd);
    return 0;
}

static int unix_request(const char *path)
{
    size_t capacity = 65536, used = 0;
    char *request = malloc(capacity);
    if (!request)
        return fail("allocate request");
    for (;;) {
        if (used == capacity) {
            char *grown = realloc(request, capacity * 2);
            if (!grown) {
                free(request);
                return fail("allocate request");
            }
            request = grown;
            capacity *= 2;
        }
        ssize_t count = read(STDIN_FILENO, request + used, capacity - used);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0) {
            free(request);
            return fail("read request");
        }
        if (count == 0)
            break;
        used += (size_t)count;
    }
    struct sockaddr_un address;
    int fd = unix_address(path, &address) == 0 ?
             socket(AF_UNIX, SOCK_STREAM, 0) : -1;
    struct timeval deadline = { RECEIVE_DEADLINE_SECONDS, 0 };
    if (fd < 0 ||
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &deadline, sizeof(deadline)) != 0 ||
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &deadline, sizeof(deadline)) != 0 ||
        connect(fd, (struct sockaddr *)&address, sizeof(address)) != 0) {
        free(request);
        if (fd >= 0)
            close(fd);
        return fail("connect");
    }
    for (size_t sent = 0; sent < used;) {
        ssize_t count = send(fd, request + sent, used - sent, 0);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0) {
            free(request);
            close(fd);
            return fail("send");
        }
        sent += (size_t)count;
    }
    free(request);
    if (shutdown(fd, SHUT_WR) != 0) {
        close(fd);
        return fail("shutdown");
    }
    char *response = malloc(RESPONSE_MAX_BYTES + 1);
    size_t received = 0;
    if (!response) {
        close(fd);
        return fail("allocate response");
    }
    for (;;) {
        ssize_t count = recv(fd, response + received,
                             RESPONSE_MAX_BYTES - received, 0);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0) {
            free(response);
            close(fd);
            return fail("receive (deadline or reset)");
        }
        if (count == 0 || (received += (size_t)count) == RESPONSE_MAX_BYTES)
            break;
    }
    close(fd);
    response[received] = '\0';
    char *end = strstr(response, "\r\n");
    size_t line = end ? (size_t)(end - response) : received;
    fwrite(response, 1, line, stdout);
    fputc('\n', stdout);
    free(response);
    return 0;
}

static int status_fail(const char *reason)
{
    fprintf(stderr, "guest-test-fixture: agent status %s\n", reason);
    return 1;
}

static int agent_status(const char *path)
{
    FILE *file = fopen(path, "rb");
    if (!file)
        return fail("open status");
    char text[65536];
    size_t length = fread(text, 1, sizeof(text), file);
    int complete = feof(file) && !ferror(file);
    fclose(file);
    if (!complete)
        return status_fail("is unreadable or too large");
    struct json_error error;
    struct json_value *status = json_parse(text, length, &error);
    if (!status) {
        fprintf(stderr, "guest-test-fixture: agent status is not JSON: %s\n",
                error.message);
        return 1;
    }
    long long protocol = 0;
    const struct json_value *docker = json_object_get(status, "dockerReady");
    const struct json_value *cri = json_object_get(status, "criReady");
    int rc = 0;
    if (status->type != JSON_OBJECT)
        rc = status_fail("is not an object");
    else if (!json_string_equals(json_object_get(status, "agentVersion"), "0.0.1"))
        rc = status_fail("agentVersion is not 0.0.1");
    else if (json_integer_value(json_object_get(status, "protocolVersion"),
                                &protocol) != 0 || protocol != 3)
        rc = status_fail("protocolVersion is not 3");
    else if (!json_string_equals(json_object_get(status, "dockerSocket"),
                                 "/var/run/docker.sock"))
        rc = status_fail("dockerSocket is wrong");
    else if (!json_string_equals(json_object_get(status, "criSocket"),
                                 "unix:///run/containerd/containerd.sock"))
        rc = status_fail("criSocket is wrong");
    else if (!json_string_equals(json_object_get(status, "kubernetesNamespace"),
                                 "k8s.io"))
        rc = status_fail("kubernetesNamespace is wrong");
    else if (!docker || (docker->type != JSON_TRUE && docker->type != JSON_FALSE))
        rc = status_fail("dockerReady is not a boolean");
    else if (!cri || (cri->type != JSON_TRUE && cri->type != JSON_FALSE))
        rc = status_fail("criReady is not a boolean");
    json_free(status);
    return rc;
}

static int mtime_ns(const char *path)
{
    struct stat info;
    if (stat(path, &info) != 0)
        return fail("stat");
#ifdef __APPLE__
    long long seconds = (long long)info.st_mtimespec.tv_sec;
    long long nanoseconds = (long long)info.st_mtimespec.tv_nsec;
#else
    long long seconds = (long long)info.st_mtim.tv_sec;
    long long nanoseconds = (long long)info.st_mtim.tv_nsec;
#endif
    printf("%lld%09lld\n", seconds, nanoseconds);
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 3 && strcmp(argv[1], "bind-unix") == 0)
        return bind_unix(argv[2]);
    if (argc == 3 && strcmp(argv[1], "unix-request") == 0)
        return unix_request(argv[2]);
    if (argc == 3 && strcmp(argv[1], "agent-status") == 0)
        return agent_status(argv[2]);
    if (argc == 3 && strcmp(argv[1], "mtime-ns") == 0)
        return mtime_ns(argv[2]);
    fprintf(stderr, "usage: guest-test-fixture bind-unix|unix-request|"
                    "agent-status|mtime-ns PATH\n");
    return 2;
}
