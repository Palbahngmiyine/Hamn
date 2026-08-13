#include "core/guest_status.h"

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>

#include "cjson/cJSON.h"

#define GUEST_STATUS_RESPONSE_CAP 4096

static int socket_path_safe(const char *path)
{
    struct stat status;
    return lstat(path, &status) == 0 && S_ISSOCK(status.st_mode) &&
           status.st_uid == geteuid() && (status.st_mode & 077) == 0;
}

static int write_all(int fd, const char *data, size_t length)
{
    while (length > 0) {
        ssize_t written = send(fd, data, length, 0);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0)
            return -1;
        data += written;
        length -= (size_t)written;
    }
    return 0;
}

int guest_status_read(const struct profile *profile, struct guest_status *status)
{
    if (!profile || !status) {
        errno = EINVAL;
        return -1;
    }
    memset(status, 0, sizeof(*status));
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, "agent.sock", path, sizeof(path)) ||
        strlen(path) >= sizeof(((struct sockaddr_un *)0)->sun_path) ||
        !socket_path_safe(path)) {
        errno = ENOENT;
        return -1;
    }
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;
    if (fcntl(fd, F_SETFD, FD_CLOEXEC) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    struct timeval timeout = { .tv_sec = 1, .tv_usec = 0 };
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    struct sockaddr_un address;
    memset(&address, 0, sizeof(address));
    address.sun_family = AF_UNIX;
    snprintf(address.sun_path, sizeof(address.sun_path), "%s", path);
    static const char request[] = "GET /v1/status HTTP/1.1\r\n"
                                  "Host: hamn\r\n"
                                  "Connection: close\r\n\r\n";
    if (connect(fd, (const struct sockaddr *)&address, sizeof(address)) != 0 ||
        write_all(fd, request, sizeof(request) - 1) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    char response[GUEST_STATUS_RESPONSE_CAP];
    size_t length = 0;
    for (;;) {
        ssize_t count = recv(fd, response + length,
                             sizeof(response) - length - 1, 0);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0) {
            int saved = errno;
            close(fd);
            errno = saved;
            return -1;
        }
        if (count == 0)
            break;
        length += (size_t)count;
        if (length == sizeof(response) - 1) {
            close(fd);
            errno = EOVERFLOW;
            return -1;
        }
    }
    close(fd);
    response[length] = '\0';
    char *body = strstr(response, "\r\n\r\n");
    if (!body || strncmp(response, "HTTP/1.1 200 ", 13) != 0) {
        errno = EPROTO;
        return -1;
    }
    body += 4;
    cJSON *json = cJSON_Parse(body);
    if (!json) {
        errno = EPROTO;
        return -1;
    }
    const cJSON *docker = cJSON_GetObjectItemCaseSensitive(json,
                                                             "dockerReady");
    const cJSON *cri = cJSON_GetObjectItemCaseSensitive(json, "criReady");
    if (!cJSON_IsBool(docker) || !cJSON_IsBool(cri)) {
        cJSON_Delete(json);
        errno = EPROTO;
        return -1;
    }
    status->available = 1;
    status->docker_api_ready = cJSON_IsTrue(docker);
    status->cri_ready = cJSON_IsTrue(cri);
    cJSON_Delete(json);
    return 0;
}
