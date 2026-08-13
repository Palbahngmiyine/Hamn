#include "fwd/docker_observer.h"

#include <arpa/inet.h>
#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/log.h"
#include "util/fs.h"
#include "util/proc.h"

#define DOCKER_HTTP_BODY_CAP (512 * 1024)
#define DOCKER_HTTP_HEADER_SLACK 8192
#define DOCKER_SNAPSHOT_MAX_CONTAINERS 2048
#define DOCKER_OBSERVER_LEASE_TOKEN_SIZE 33

static int object_unique_item(const cJSON *object, const char *name,
                              const cJSON **item)
{
    const cJSON *found = NULL;
    if (!cJSON_IsObject(object))
        return -1;
    for (const cJSON *child = object->child; child; child = child->next) {
        if (!child->string || strcmp(child->string, name) != 0)
            continue;
        if (found)
            return -1;
        found = child;
    }
    *item = found;
    return 0;
}

static int inspect_port_key(const char *text, unsigned *container_port,
                            enum port_protocol *protocol)
{
    const char *slash = text ? strrchr(text, '/') : NULL;
    if (!slash || slash == text || slash[1] == '\0')
        return -1;
    char port_text[16];
    size_t length = (size_t)(slash - text);
    if (length >= sizeof(port_text))
        return -1;
    memcpy(port_text, text, length);
    port_text[length] = '\0';
    if (port_number_parse(port_text, container_port) != 0)
        return -1;
    if (strcmp(slash + 1, "tcp") == 0)
        *protocol = PORT_TCP;
    else if (strcmp(slash + 1, "udp") == 0)
        *protocol = PORT_UDP;
    else
        return 1;
    return 0;
}

static int append_spec(struct port_spec specs[], int *count, size_t capacity,
                       const struct port_spec *candidate)
{
    if (*count < 0 || (size_t)*count >= capacity ||
        *count >= DOCKER_OBSERVER_MAX_PORTS) {
        errno = EOVERFLOW;
        return -1;
    }
    for (int i = 0; i < *count; i++) {
        if (specs[i].protocol == candidate->protocol &&
            specs[i].host_port == candidate->host_port) {
            errno = EPROTO;
            return -1;
        }
    }
    specs[(*count)++] = *candidate;
    return 0;
}

static int append_binding(const cJSON *binding, unsigned container_port,
                          enum port_protocol protocol,
                          struct port_spec specs[], int *count,
                          size_t capacity)
{
    const cJSON *host_ip = NULL;
    const cJSON *host_port = NULL;
    if (object_unique_item(binding, "HostIp", &host_ip) != 0 ||
        object_unique_item(binding, "HostPort", &host_port) != 0 ||
        !cJSON_IsString(host_ip) || !cJSON_IsString(host_port)) {
        errno = EPROTO;
        return -1;
    }
    struct port_spec candidate = {
        .container_port = container_port,
        .protocol = protocol,
    };
    const char *ip = host_ip->valuestring;
    if (!ip[0])
        ip = "0.0.0.0";
    struct in_addr address;
    if (inet_pton(AF_INET, ip, &address) != 1) {
        /* Docker can report a separate IPv6 bind; Hamn has IPv4 relays. */
        if (strchr(ip, ':'))
            return 0;
        errno = EPROTO;
        return -1;
    }
    if (strlen(ip) >= sizeof(candidate.host_ip) ||
        port_number_parse(host_port->valuestring, &candidate.host_port) != 0) {
        errno = EPROTO;
        return -1;
    }
    snprintf(candidate.host_ip, sizeof(candidate.host_ip), "%s", ip);
    return append_spec(specs, count, capacity, &candidate);
}

int docker_observer_parse_inspect(const char *json,
                                  struct port_spec specs[], int *count,
                                  size_t capacity)
{
    if (!json || !specs || !count || capacity == 0 ||
        capacity > DOCKER_OBSERVER_MAX_PORTS || *count < 0 ||
        (size_t)*count > capacity) {
        errno = EINVAL;
        return -1;
    }
    struct port_spec staged[DOCKER_OBSERVER_MAX_PORTS];
    memcpy(staged, specs, (size_t)*count * sizeof(staged[0]));
    int staged_count = *count;

    const char *end = NULL;
    cJSON *root = cJSON_ParseWithOpts(json, &end, 1);
    const cJSON *network = NULL;
    const cJSON *ports = NULL;
    int result = -1;
    if (!root || !end || *end != '\0' ||
        object_unique_item(root, "NetworkSettings", &network) != 0 ||
        object_unique_item(network, "Ports", &ports) != 0 ||
        !cJSON_IsObject(ports)) {
        errno = EPROTO;
        goto out;
    }

    for (const cJSON *entry = ports->child; entry; entry = entry->next) {
        unsigned container_port = 0;
        enum port_protocol protocol;
        int key = inspect_port_key(entry->string, &container_port, &protocol);
        if (key < 0 || !entry->string) {
            errno = EPROTO;
            goto out;
        }
        for (const cJSON *previous = ports->child; previous != entry;
             previous = previous->next) {
            if (previous->string && strcmp(previous->string, entry->string) == 0) {
                errno = EPROTO;
                goto out;
            }
        }
        if (key > 0 || cJSON_IsNull(entry))
            continue;
        if (!cJSON_IsArray(entry)) {
            errno = EPROTO;
            goto out;
        }
        for (const cJSON *binding = entry->child; binding;
             binding = binding->next) {
            if (append_binding(binding, container_port, protocol, staged,
                               &staged_count, capacity) != 0)
                goto out;
        }
    }
    memcpy(specs, staged, (size_t)staged_count * sizeof(specs[0]));
    *count = staged_count;
    result = 0;

out:
    cJSON_Delete(root);
    return result;
}

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

static const char *find_crlf(const char *cursor, const char *limit)
{
    for (; cursor + 1 < limit; cursor++) {
        if (cursor[0] == '\r' && cursor[1] == '\n')
            return cursor;
    }
    return NULL;
}

static int headers_use_chunked(const char *response, const char *header_end)
{
    const char *line = find_crlf(response, header_end);
    if (!line)
        return -1;
    line += 2;
    while (line < header_end) {
        const char *line_end = find_crlf(line, header_end);
        if (!line_end)
            return -1;
        static const char prefix[] = "Transfer-Encoding:";
        size_t prefix_length = sizeof(prefix) - 1;
        if ((size_t)(line_end - line) >= prefix_length &&
            strncasecmp(line, prefix, prefix_length) == 0) {
            const char *value = line + prefix_length;
            while (value < line_end && (*value == ' ' || *value == '\t'))
                value++;
            return (size_t)(line_end - value) == 7 &&
                   strncasecmp(value, "chunked", 7) == 0 ? 1 : -1;
        }
        line = line_end + 2;
    }
    return 0;
}

static int chunked_body_decode(const char *input, size_t input_length,
                               char body[DOCKER_HTTP_BODY_CAP])
{
    const char *cursor = input;
    const char *limit = input + input_length;
    size_t output = 0;
    while (cursor < limit) {
        const char *line_end = find_crlf(cursor, limit);
        unsigned long long chunk = 0;
        int digits = 0;
        if (!line_end)
            break;
        for (const char *p = cursor; p < line_end && *p != ';'; p++) {
            unsigned char value;
            if (*p >= '0' && *p <= '9')
                value = (unsigned char)(*p - '0');
            else if (*p >= 'a' && *p <= 'f')
                value = (unsigned char)(*p - 'a' + 10);
            else if (*p >= 'A' && *p <= 'F')
                value = (unsigned char)(*p - 'A' + 10);
            else {
                errno = EPROTO;
                return -1;
            }
            if (chunk > (ULLONG_MAX - value) / 16) {
                errno = EOVERFLOW;
                return -1;
            }
            chunk = chunk * 16 + value;
            digits = 1;
        }
        if (!digits) {
            errno = EPROTO;
            return -1;
        }
        cursor = line_end + 2;
        if (chunk == 0 && cursor + 2 == limit &&
            cursor[0] == '\r' && cursor[1] == '\n') {
            body[output] = '\0';
            return 0;
        }
        if (chunk == 0 || chunk > (unsigned long long)(limit - cursor) ||
            chunk >= DOCKER_HTTP_BODY_CAP - output ||
            cursor + chunk + 2 > limit || cursor[chunk] != '\r' ||
            cursor[chunk + 1] != '\n') {
            errno = chunk >= DOCKER_HTTP_BODY_CAP - output ? EOVERFLOW : EPROTO;
            return -1;
        }
        memcpy(body + output, cursor, (size_t)chunk);
        output += (size_t)chunk;
        cursor += (size_t)chunk + 2;
    }
    errno = EPROTO;
    return -1;
}

static int docker_http_get(const char *socket_path, const char *target,
                           char body[DOCKER_HTTP_BODY_CAP], int timeout_sec)
{
    if (!socket_path_safe(socket_path) ||
        strlen(socket_path) >= sizeof(((struct sockaddr_un *)0)->sun_path)) {
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
    struct timeval timeout = { .tv_sec = timeout_sec, .tv_usec = 0 };
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    snprintf(address.sun_path, sizeof(address.sun_path), "%s", socket_path);
    char request[256];
    int request_length = snprintf(request, sizeof(request),
                                  "GET %s HTTP/1.1\r\nHost: hamn\r\n"
                                  "Connection: close\r\n\r\n", target);
    if (request_length < 0 || request_length >= (int)sizeof(request) ||
        connect(fd, (struct sockaddr *)&address, sizeof(address)) != 0 ||
        write_all(fd, request, (size_t)request_length) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    size_t capacity = DOCKER_HTTP_BODY_CAP + DOCKER_HTTP_HEADER_SLACK;
    char *response = calloc(capacity + 1, 1);
    if (!response) {
        close(fd);
        return -1;
    }
    size_t length = 0;
    for (;;) {
        ssize_t received = recv(fd, response + length, capacity - length, 0);
        if (received < 0 && errno == EINTR)
            continue;
        if (received < 0 || (received == 0 && length == 0) ||
            (received > 0 && (length += (size_t)received) == capacity)) {
            int saved = received < 0 ? errno : EOVERFLOW;
            close(fd);
            free(response);
            errno = saved;
            return -1;
        }
        if (received == 0)
            break;
    }
    close(fd);
    response[length] = '\0';
    char *payload = strstr(response, "\r\n\r\n");
    int chunked = payload ? headers_use_chunked(response, payload + 2) : -1;
    if (!payload || strncmp(response, "HTTP/1.1 200 ", 13) != 0 ||
        chunked < 0) {
        free(response);
        errno = EPROTO;
        return -1;
    }
    payload += 4;
    size_t payload_length = length - (size_t)(payload - response);
    if (chunked) {
        int result = chunked_body_decode(payload, payload_length, body);
        free(response);
        return result;
    }
    if (payload_length >= DOCKER_HTTP_BODY_CAP) {
        free(response);
        errno = EOVERFLOW;
        return -1;
    }
    memcpy(body, payload, payload_length);
    body[payload_length] = '\0';
    free(response);
    return 0;
}

static int container_id_valid(const char *id)
{
    size_t length = id ? strlen(id) : 0;
    if (length < 12 || length > 64)
        return 0;
    for (size_t i = 0; i < length; i++) {
        if (!isxdigit((unsigned char)id[i]))
            return 0;
    }
    return 1;
}

int docker_observer_read_snapshot(const struct profile *profile,
                                  struct port_spec specs[], int *count,
                                  size_t capacity)
{
    if (!profile || !specs || !count || capacity == 0 ||
        capacity > DOCKER_OBSERVER_MAX_PORTS) {
        errno = EINVAL;
        return -1;
    }
    char socket_path[PATH_MAX], list[DOCKER_HTTP_BODY_CAP];
    if (!profile_path(profile, "docker.sock", socket_path,
                      sizeof(socket_path)) ||
        docker_http_get(socket_path, "/containers/json", list, 3) != 0)
        return -1;
    const char *end = NULL;
    cJSON *containers = cJSON_ParseWithOpts(list, &end, 1);
    if (!containers || !end || *end != '\0' || !cJSON_IsArray(containers) ||
        cJSON_GetArraySize(containers) > DOCKER_SNAPSHOT_MAX_CONTAINERS) {
        cJSON_Delete(containers);
        errno = EPROTO;
        return -1;
    }
    struct port_spec staged[DOCKER_OBSERVER_MAX_PORTS];
    int staged_count = 0;
    for (const cJSON *container = containers->child; container;
         container = container->next) {
        const cJSON *id = NULL;
        if (object_unique_item(container, "Id", &id) != 0 ||
            !cJSON_IsString(id) || !container_id_valid(id->valuestring)) {
            cJSON_Delete(containers);
            errno = EPROTO;
            return -1;
        }
        char target[96], inspect[DOCKER_HTTP_BODY_CAP];
        int target_length = snprintf(target, sizeof(target),
                                     "/containers/%s/json", id->valuestring);
        if (target_length < 0 || target_length >= (int)sizeof(target) ||
            docker_http_get(socket_path, target, inspect, 3) != 0 ||
            docker_observer_parse_inspect(inspect, staged, &staged_count,
                                          capacity) != 0) {
            cJSON_Delete(containers);
            return -1;
        }
    }
    cJSON_Delete(containers);
    memcpy(specs, staged, (size_t)staged_count * sizeof(specs[0]));
    *count = staged_count;
    return 0;
}

static int lease_token_valid(const char *token)
{
    if (!token || strlen(token) != DOCKER_OBSERVER_LEASE_TOKEN_SIZE - 1)
        return 0;
    for (size_t i = 0; token[i]; i++) {
        if (!((token[i] >= '0' && token[i] <= '9') ||
              (token[i] >= 'a' && token[i] <= 'f')))
            return 0;
    }
    return 1;
}

static int observer_lease_path(const struct profile *profile, char *path,
                               size_t capacity)
{
    return profile && profile_path(profile, "port-observer.lease", path,
                                   capacity) != NULL;
}

static int observer_lease_matches(const struct profile *profile,
                                  const char *token)
{
    char path[PATH_MAX];
    if (!lease_token_valid(token) || !observer_lease_path(profile, path,
                                                          sizeof(path)))
        return 0;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return 0;
    struct stat status;
    char actual[DOCKER_OBSERVER_LEASE_TOKEN_SIZE + 1] = {0};
    size_t expected = strlen(token) + 1;
    ssize_t received;
    do {
        received = read(fd, actual, expected);
    } while (received < 0 && errno == EINTR);
    char extra = '\0';
    ssize_t after;
    do {
        after = read(fd, &extra, 1);
    } while (after < 0 && errno == EINTR);
    int valid = fstat(fd, &status) == 0 && S_ISREG(status.st_mode) &&
        status.st_uid == geteuid() && (status.st_mode & 077) == 0 &&
        status.st_nlink == 1 && status.st_size == (off_t)expected &&
        received == (ssize_t)expected && after == 0 &&
        memcmp(actual, token, expected - 1) == 0 && actual[expected - 1] == '\n';
    close(fd);
    return valid;
}

int docker_observer_revoke(const struct profile *profile)
{
    char path[PATH_MAX];
    if (!observer_lease_path(profile, path, sizeof(path))) {
        errno = EINVAL;
        return -1;
    }
    return fs_unlink_if_exists(path);
}

int docker_observer_sync_once(const struct profile *profile,
                              const char *guest_ip, const char *lease)
{
    if (!profile || !guest_ip || !guest_ip[0] || !lease_token_valid(lease)) {
        errno = EINVAL;
        return -1;
    }
    if (!observer_lease_matches(profile, lease))
        return 1;
    struct port_spec specs[DOCKER_OBSERVER_MAX_PORTS];
    int spec_count = 0;
    if (docker_observer_read_snapshot(profile, specs, &spec_count,
                                      DOCKER_OBSERVER_MAX_PORTS) != 0)
        return -1;
    int operation_lock = port_forward_operation_lock(profile);
    if (operation_lock < 0)
        return -1;
    int result = observer_lease_matches(profile, lease) ?
        port_forward_sync_docker_serialized(profile, guest_ip, specs,
                                            spec_count) : 1;
    port_forward_operation_unlock(operation_lock);
    return result;
}

static void lease_token_generate(char token[DOCKER_OBSERVER_LEASE_TOKEN_SIZE])
{
    unsigned char random[16];
    static const char digits[] = "0123456789abcdef";
    arc4random_buf(random, sizeof(random));
    for (size_t i = 0; i < sizeof(random); i++) {
        token[i * 2] = digits[random[i] >> 4];
        token[i * 2 + 1] = digits[random[i] & 0x0f];
    }
    token[DOCKER_OBSERVER_LEASE_TOKEN_SIZE - 1] = '\0';
}

static int observer_lease_write(const struct profile *profile,
                                const char *token)
{
    char path[PATH_MAX], text[DOCKER_OBSERVER_LEASE_TOKEN_SIZE + 1];
    if (!lease_token_valid(token) || !observer_lease_path(profile, path,
                                                          sizeof(path))) {
        errno = EINVAL;
        return -1;
    }
    int length = snprintf(text, sizeof(text), "%s\n", token);
    return length == DOCKER_OBSERVER_LEASE_TOKEN_SIZE ?
        fs_write_file_atomic(path, text, (size_t)length, 0600) : -1;
}

static void observer_delay_milliseconds(long milliseconds)
{
    struct timespec delay = {
        .tv_sec = milliseconds / 1000,
        .tv_nsec = (milliseconds % 1000) * 1000000,
    };
    while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {}
}

static int docker_observer_wait_events(const struct profile *profile)
{
    char socket_path[PATH_MAX], target[128], events[DOCKER_HTTP_BODY_CAP];
    time_t now = time(NULL);
    if (now < 0 || !profile_path(profile, "docker.sock", socket_path,
                                 sizeof(socket_path)))
        return -1;
    int length = snprintf(target, sizeof(target),
                          "/events?since=%lld&until=%lld",
                          (long long)now, (long long)now + 2);
    if (length < 0 || length >= (int)sizeof(target) ||
        docker_http_get(socket_path, target, events, 4) != 0)
        return -1;
    return events[0] ? 1 : 0;
}

int docker_observer_watch(const struct profile *profile, const char *guest_ip,
                          const char *lease, unsigned cycle_limit)
{
    unsigned cycles = 0;
    for (;;) {
        int sync = docker_observer_sync_once(profile, guest_ip, lease);
        if (sync == 1)
            return 0;
        if (sync != 0) {
            logerr("Docker port observer snapshot failed; retrying");
            observer_delay_milliseconds(500);
            continue;
        }
        if (!observer_lease_matches(profile, lease))
            return 0;
        int events = docker_observer_wait_events(profile);
        if (events < 0) {
            if (!observer_lease_matches(profile, lease))
                return 0;
            logerr("Docker port observer event stream failed; retrying");
            observer_delay_milliseconds(500);
        } else if (events == 0) {
            observer_delay_milliseconds(100);
        }
        if (cycle_limit && ++cycles >= cycle_limit)
            return 0;
    }
}

struct observer_options {
    const char *profile;
    const char *ip;
    const char *lease;
};

static int observer_options_parse(int argc, char **argv,
                                  struct observer_options *options)
{
    memset(options, 0, sizeof(*options));
    static const struct option long_options[] = {
        { "profile", required_argument, NULL, 'p' },
        { "ip", required_argument, NULL, 'i' },
        { "lease", required_argument, NULL, 'l' },
        { 0 },
    };
    optind = 1;
    optreset = 1;
    int option;
    while ((option = getopt_long(argc, argv, "p:i:l:", long_options,
                                 NULL)) != -1) {
        if ((option == 'p' && !options->profile) ||
            (option == 'i' && !options->ip) ||
            (option == 'l' && !options->lease)) {
            if (option == 'p')
                options->profile = optarg;
            else if (option == 'i')
                options->ip = optarg;
            else
                options->lease = optarg;
            continue;
        }
        return -1;
    }
    struct in_addr address;
    return optind == argc && profile_name_valid(options->profile) &&
        lease_token_valid(options->lease) && options->ip &&
        inet_pton(AF_INET, options->ip, &address) == 1 ? 0 : -1;
}

int cmd_port_observer(int argc, char **argv)
{
    struct observer_options options;
    if (observer_options_parse(argc, argv, &options) != 0)
        return 2;
    struct profile profile;
    if (profile_load(&profile, options.profile) != 0) {
        logerr("cannot load Docker observer profile %s", options.profile);
        return 1;
    }
    return docker_observer_watch(&profile, options.ip, options.lease, 0) == 0 ?
           0 : 1;
}

int docker_observer_start(const struct profile *profile,
                          const struct vm_state *state)
{
    if (!profile || !state || !state->ip[0]) {
        errno = EINVAL;
        return -1;
    }
    char token[DOCKER_OBSERVER_LEASE_TOKEN_SIZE], self[PATH_MAX];
    char logs[PATH_MAX], logfile[PATH_MAX];
    lease_token_generate(token);
    if (observer_lease_write(profile, token) != 0 ||
        !proc_self_path(self, sizeof(self)) ||
        !profile_path(profile, "logs", logs, sizeof(logs)) ||
        fs_mkdirs(logs, 0755) != 0 ||
        !profile_path(profile, "logs/port-observer.log", logfile,
                      sizeof(logfile))) {
        (void)docker_observer_revoke(profile);
        return -1;
    }
    const char *argv[] = {
        self, "port-observer", "--profile", profile->name,
        "--ip", state->ip, "--lease", token, NULL,
    };
    if (proc_spawn_daemon(argv, logfile) < 0) {
        (void)docker_observer_revoke(profile);
        return -1;
    }
    return 0;
}
