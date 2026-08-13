#include "api/router.h"

#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "api/cri_status.h"
#include "api/mount_inotify.h"
#include "http/http.h"
#include "http/resp.h"
#include "version.h"

#define DOCKER_SOCKET "/var/run/docker.sock"
#define STATUS_SOCKET_TIMEOUT_USEC 250000
static int docker_api_ready(void)
{
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0)
        return 0;
    /* Reserve the rest of the status budget for CRI plugin verification. */
    struct timeval timeout = { .tv_sec = 0,
                               .tv_usec = STATUS_SOCKET_TIMEOUT_USEC };
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    snprintf(sa.sun_path, sizeof(sa.sun_path), "%s", DOCKER_SOCKET);
    static const char request[] = "GET /_ping HTTP/1.1\r\n"
                                  "Host: localhost\r\n"
                                  "Connection: close\r\n\r\n";
    if (connect(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0) {
        close(fd);
        return 0;
    }
    size_t sent = 0;
    while (sent < sizeof(request) - 1) {
        ssize_t count;
        do {
            count = send(fd, request + sent, sizeof(request) - 1 - sent, 0);
        } while (count < 0 && errno == EINTR);
        if (count <= 0) {
            close(fd);
            return 0;
        }
        sent += (size_t)count;
    }
    char response[128];
    ssize_t count;
    do {
        count = recv(fd, response, sizeof(response) - 1, 0);
    } while (count < 0 && errno == EINTR);
    close(fd);
    if (count <= 0)
        return 0;
    response[count] = '\0';
    return strncmp(response, "HTTP/1.1 200 ", 13) == 0 ||
           strncmp(response, "HTTP/1.0 200 ", 13) == 0;
}

static void status_response(struct conn *c)
{
    cJSON *j = cJSON_CreateObject();
    cJSON_AddStringToObject(j, "agentVersion", HAMND_VERSION);
    cJSON_AddNumberToObject(j, "protocolVersion",
                           HAMND_STATUS_PROTOCOL_VERSION);
    cJSON_AddStringToObject(j, "dockerSocket", DOCKER_SOCKET);
    cJSON_AddBoolToObject(j, "dockerReady", docker_api_ready());
    cJSON_AddStringToObject(j, "criSocket", "unix://" HAMND_CONTAINERD_SOCKET);
    cJSON_AddBoolToObject(j, "criReady", cri_plugin_ready());
    cJSON_AddStringToObject(j, "kubernetesNamespace", "k8s.io");
    resp_json(c, 200, j);
}

static int json_object_exact_keys(const cJSON *object,
                                  const char *const keys[], size_t count)
{
    if (!cJSON_IsObject(object))
        return 0;
    size_t actual = 0;
    for (const cJSON *child = object->child; child; child = child->next) {
        if (!child->string)
            return 0;
        size_t matches = 0;
        for (size_t index = 0; index < count; index++) {
            if (strcmp(child->string, keys[index]) == 0)
                matches++;
        }
        if (matches != 1)
            return 0;
        for (const cJSON *other = child->next; other; other = other->next) {
            if (other->string && strcmp(child->string, other->string) == 0)
                return 0;
        }
        actual++;
    }
    return actual == count;
}

static int json_nonnegative_integer(const cJSON *value, long long maximum,
                                    long long *out)
{
    if (!cJSON_IsNumber(value) || value->valuedouble < 0 ||
        value->valuedouble > (double)maximum)
        return -1;
    long long converted = (long long)value->valuedouble;
    if ((double)converted != value->valuedouble)
        return -1;
    *out = converted;
    return 0;
}

static void mount_inotify_response(struct conn *c, const struct http_req *r)
{
    static const char *const keys[] = {
        "tag", "path", "mtimeSec", "mtimeNsec",
    };
    if (r->body_len == 0 || r->body_len > 16384) {
        resp_error(c, 400, "mountInotify request body is invalid");
        return;
    }
    const char *end = NULL;
    cJSON *body = cJSON_ParseWithLengthOpts(r->body, r->body_len, &end, 0);
    if (!body || end != r->body + r->body_len ||
        !json_object_exact_keys(body, keys, sizeof(keys) / sizeof(keys[0]))) {
        cJSON_Delete(body);
        resp_error(c, 400, "mountInotify request JSON is invalid");
        return;
    }
    const cJSON *tag = cJSON_GetObjectItemCaseSensitive(body, "tag");
    const cJSON *path = cJSON_GetObjectItemCaseSensitive(body, "path");
    const cJSON *seconds = cJSON_GetObjectItemCaseSensitive(body, "mtimeSec");
    const cJSON *nanoseconds =
        cJSON_GetObjectItemCaseSensitive(body, "mtimeNsec");
    long long seconds_value, nanoseconds_value;
    if (!cJSON_IsString(tag) || !cJSON_IsString(path) ||
        json_nonnegative_integer(seconds, 4102444800LL, &seconds_value) != 0 ||
        json_nonnegative_integer(nanoseconds, 999999999LL,
                                 &nanoseconds_value) != 0) {
        cJSON_Delete(body);
        resp_error(c, 400, "mountInotify request fields are invalid");
        return;
    }
    int rc = mount_inotify_touch(tag->valuestring, path->valuestring,
                                 (time_t)seconds_value,
                                 (long)nanoseconds_value);
    int saved = errno;
    cJSON_Delete(body);
    if (rc == 0) {
        resp_empty(c, 204);
        return;
    }
    if (saved == ENOENT) {
        resp_error(c, 404, "mountInotify target does not exist");
        return;
    }
    if (saved == EINVAL || saved == ELOOP || saved == ENAMETOOLONG) {
        resp_error(c, 400, "mountInotify target is unsafe");
        return;
    }
    resp_error(c, 409, "mountInotify target cannot be refreshed");
}

void router_dispatch(struct conn *c, struct http_req *r)
{
    if (strcmp(r->path, "/_ping") == 0 &&
        (strcmp(r->method, "HEAD") == 0 ||
         strcmp(r->method, "GET") == 0)) {
        if (strcmp(r->method, "HEAD") == 0)
            resp_empty(c, 200);
        else
            resp_text(c, 200, "OK");
        return;
    }
    if (strcmp(r->method, "GET") == 0 &&
        (strcmp(r->path, "/status") == 0 ||
         strcmp(r->path, "/v1/status") == 0)) {
        status_response(c);
        return;
    }
    if (strcmp(r->method, "POST") == 0 &&
        strcmp(r->path, "/v1/mount-inotify") == 0) {
        mount_inotify_response(c, r);
        return;
    }
    resp_error(c, 404, "agent endpoint not found");
}
