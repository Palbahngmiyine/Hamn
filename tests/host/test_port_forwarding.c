#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/log.h"
#include "core/profile.h"
#include "fwd/docker_observer.h"
#include "fwd/ports.h"
#include "fwd/udp_proxy.h"
#include "sshmgr/ssh.h"
#include "util/fs.h"
#include "util/proc.h"

static void append_event(const char *operation, const char *bind_address,
                         unsigned local_port)
{
    const char *path = getenv("PORT_TEST_EVENTS");
    if (!path)
        return;
    char line[256];
    int length = snprintf(line, sizeof(line), "%s\t%s\t%u\n", operation,
                          bind_address, local_port);
    if (length < 0 || length >= (int)sizeof(line))
        return;
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (fd >= 0) {
        (void)write(fd, line, (size_t)length);
        close(fd);
    }
}

static int configured_port(const char *name, unsigned local_port)
{
    const char *value = getenv(name);
    if (!value)
        return 0;
    char text[16];
    snprintf(text, sizeof(text), "%u", local_port);
    return strcmp(value, text) == 0;
}

int ssh_forward_add_tcp(const struct profile *p, const char *ip,
                        const char *bind_address, unsigned local_port,
                        const char *remote_address, unsigned remote_port)
{
    (void)p;
    (void)ip;
    (void)remote_address;
    (void)remote_port;
    append_event("add", bind_address, local_port);
    return configured_port("FAIL_FORWARD_PORT", local_port) ? -1 : 0;
}

int ssh_forward_add_tcp_observed(const struct profile *p, const char *ip,
                                 const char *bind_address,
                                 unsigned local_port,
                                 const char *remote_address,
                                 unsigned remote_port,
                                 ssh_forward_completion_fn completion,
                                 void *context)
{
    int rc = ssh_forward_add_tcp(p, ip, bind_address, local_port,
                                 remote_address, remote_port);
    if (completion && completion(rc, context) != 0)
        return -1;
    return rc;
}

int ssh_forward_cancel_tcp(const struct profile *p, const char *ip,
                           const char *bind_address, unsigned local_port,
                           const char *remote_address, unsigned remote_port)
{
    (void)p;
    (void)ip;
    (void)remote_address;
    (void)remote_port;
    append_event("cancel", bind_address, local_port);
    return configured_port("FAIL_CANCEL_PORT", local_port) ? -1 : 0;
}

const char *ssh_forward_add_detail(void)
{
    return "";
}

const char *ssh_forward_cancel_detail(void)
{
    return "";
}

int ssh_master_alive(const struct profile *p)
{
    (void)p;
    return getenv("SSH_MASTER_GONE") ? -1 : 0;
}

const char *profile_path(const struct profile *p, const char *file, char *buf,
                         size_t cap)
{
    snprintf(buf, cap, "%s/%s", p->dir, file);
    return buf;
}

int profile_name_valid(const char *name)
{
    return name && name[0] && !strchr(name, '/');
}

int profile_load(struct profile *profile, const char *name)
{
    (void)profile;
    (void)name;
    errno = ENOSYS;
    return -1;
}

void logmsg(const char *fmt, ...)
{
    (void)fmt;
}

void logerr(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
}

void die(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
    exit(1);
}

static int load_spec(const char *text, struct port_spec *spec)
{
    char error[160];
    if (port_spec_parse(text, spec, error, sizeof(error)) == 0)
        return 0;
    fprintf(stderr, "invalid test port specification: %s\n", error);
    return -1;
}

/* Announces on PORT_TEST_READY_FIFO that the suite may act and waits for a
 * byte on PORT_TEST_RELEASE_FIFO. Neither descriptor reaches a child. */
static int suite_rendezvous(void)
{
    const char *ready = getenv("PORT_TEST_READY_FIFO");
    const char *release = getenv("PORT_TEST_RELEASE_FIFO");
    if (!ready || !release)
        return -1;
    int fd = open(ready, O_WRONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    int announced = write(fd, "ready\n", 6) == 6;
    close(fd);
    fd = announced ? open(release, O_RDONLY | O_CLOEXEC) : -1;
    if (fd < 0)
        return -1;
    char byte;
    ssize_t received;
    do {
        received = read(fd, &byte, 1);
    } while (received < 0 && errno == EINTR);
    close(fd);
    return received == 1 ? 0 : -1;
}

static int ignore_sigterm(void)
{
    const char *ready = getenv("PORT_TEST_READY_FIFO");
    if (!ready || signal(SIGTERM, SIG_IGN) == SIG_ERR)
        return 1;
    int fd = open(ready, O_WRONLY | O_CLOEXEC);
    if (fd < 0)
        return 1;
    int rc = write(fd, "ready\n", 6) == 6 ? 0 : 1;
    close(fd);
    if (rc != 0)
        return rc;
    for (;;)
        pause();
}

static int spawn_ignore_sigterm(void)
{
    const char *directory = getenv("PORT_TEST_DIR");
    char self[PATH_MAX], logfile[PATH_MAX];
    if (!directory || !proc_self_path(self, sizeof(self)))
        return 1;
    int length = snprintf(logfile, sizeof(logfile), "%s/logs/ignore.log",
                          directory);
    if (length < 0 || length >= (int)sizeof(logfile))
        return 1;
    const char *argv[] = { self, "ignore-sigterm", NULL };
    pid_t pid = proc_spawn_daemon(argv, logfile);
    if (pid <= 1)
        return 1;
    printf("%d\n", pid);
    return 0;
}

static int inspect_parser_fixtures(void)
{
    static const char valid[] =
        "{\"NetworkSettings\":{\"Ports\":{"
        "\"80/tcp\":[{\"HostIp\":\"127.0.0.1\",\"HostPort\":\"48240\"}],"
        "\"53/udp\":[{\"HostIp\":\"0.0.0.0\",\"HostPort\":\"48241\"}],"
        "\"443/sctp\":null}}}";
    static const char duplicate_key[] =
        "{\"NetworkSettings\":{\"Ports\":{"
        "\"80/tcp\":[{\"HostIp\":\"127.0.0.1\",\"HostPort\":\"48242\"}],"
        "\"80/tcp\":[{\"HostIp\":\"127.0.0.1\",\"HostPort\":\"48243\"}]}}}";
    static const char malformed_binding[] =
        "{\"NetworkSettings\":{\"Ports\":{"
        "\"80/tcp\":[{\"HostIp\":\"127.0.0.1\",\"HostPort\":42}]}}}";
    struct port_spec specs[DOCKER_OBSERVER_MAX_PORTS] = {0};
    int count = 0;
    if (docker_observer_parse_inspect(valid, specs, &count,
                                      DOCKER_OBSERVER_MAX_PORTS) != 0 ||
        count != 2 || specs[0].protocol != PORT_TCP ||
        specs[0].host_port != 48240 || specs[0].container_port != 80 ||
        strcmp(specs[0].host_ip, "127.0.0.1") != 0 ||
        specs[1].protocol != PORT_UDP || specs[1].host_port != 48241 ||
        specs[1].container_port != 53 ||
        strcmp(specs[1].host_ip, "0.0.0.0") != 0)
        return 1;
    struct port_spec saved[DOCKER_OBSERVER_MAX_PORTS];
    memcpy(saved, specs, sizeof(saved));
    if (docker_observer_parse_inspect(duplicate_key, specs, &count,
                                      DOCKER_OBSERVER_MAX_PORTS) == 0 ||
        docker_observer_parse_inspect(malformed_binding, specs, &count,
                                      DOCKER_OBSERVER_MAX_PORTS) == 0 ||
        count != 2 || memcmp(saved, specs, sizeof(saved)) != 0)
        return 1;
    return 0;
}

static int list_parser_fixtures(void)
{
    const char *valid = "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]";
    const char *invalid[] = {
        "{}",
        "[",
        "null",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": null}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}, {\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\": \"../bad\", \"Ports\": []}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"invalid:ip\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": false, \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": 0, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": 65536, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": 1.5, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": \"80\", \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 0, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"Ports\": [{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"unknown\"}, {\"PrivatePort\": 81, \"Type\": \"tcp\"}, {\"IP\": \"::\", \"PrivatePort\": 80, \"PublicPort\": 48240, \"Type\": \"tcp\"}, {\"IP\": \"0.0.0.0\", \"PrivatePort\": 53, \"PublicPort\": 48241, \"Type\": \"udp\"}]}]",
        "[{\"Id\":\"aaaaaaaaaaaa\",\"Ports\":[],\"Ports\":[]}]",
        "[{\"Id\":\"aaaaaaaaaaaa\",\"Id\":\"bbbbbbbbbbbb\",\"Ports\":[]}]",
        "[{\"Id\":\"aaaaaaaaaaaa\",\"Ports\":[{\"IP\":\"0.0.0.0\",\"PrivatePort\":80,\"PublicPort\":80,\"PublicPort\":81,\"Type\":\"tcp\"}]}]",
    };
    struct port_spec specs[DOCKER_OBSERVER_MAX_PORTS] = {0};
    int count = 0;
    if (docker_observer_parse_list(valid, specs, &count, DOCKER_OBSERVER_MAX_PORTS) ||
        count != 2 || specs[0].host_port != 48240 || specs[0].container_port != 80 ||
        specs[0].protocol != PORT_TCP || strcmp(specs[0].host_ip, "127.0.0.1") ||
        specs[1].protocol != PORT_UDP || specs[1].host_port != 48241)
        return 1;
    struct port_spec saved[DOCKER_OBSERVER_MAX_PORTS];
    memcpy(saved, specs, sizeof(saved));
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); i++) {
        if (docker_observer_parse_list(invalid[i], specs, &count, DOCKER_OBSERVER_MAX_PORTS) == 0 ||
            count != 2 || memcmp(saved, specs, sizeof(saved)))
            return 1;
    }
    if (docker_observer_parse_list(valid, specs, &count, 1) == 0 ||
        count != 2 || memcmp(saved, specs, sizeof(saved)))
        return 1;
    if (docker_observer_parse_list("[]", specs, &count, DOCKER_OBSERVER_MAX_PORTS) || count)
        return 1;
    return 0;
}

static int fixture_write_all(int fd, const char *text, size_t length)
{
    while (length > 0) {
        ssize_t written = write(fd, text, length);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0)
            return -1;
        text += written;
        length -= (size_t)written;
    }
    return 0;
}

/* Stands in for a new SSH master: the control socket becomes another file. */
static int fixture_replace_master_socket(const char *path)
{
    if (unlink(path) != 0 && errno != ENOENT)
        return -1;
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    return fd >= 0 && close(fd) == 0 ? 0 : -1;
}

/* Serves `cycles` observer passes, each a container list and an event. The
 * master socket is replaced while the list of pass `replace_at_cycle`
 * (counted from 1, 0 for never) is requested, before that pass looks at it. */
static int snapshot_fixture_server(const char *path, int ready_fd,
                                   unsigned cycles, const char *master_socket,
                                   unsigned replace_at_cycle)
{
    static const char *const bodies[] = {
        "[{\"Id\":\"aaaaaaaaaaaa\",\"Ports\":[{\"IP\":\"127.0.0.1\",\"PrivatePort\":80,\"PublicPort\":48250,\"Type\":\"tcp\"}]},"
        "{\"Id\":\"bbbbbbbbbbbb\",\"Ports\":[{\"IP\":\"0.0.0.0\",\"PrivatePort\":53,\"PublicPort\":48251,\"Type\":\"udp\"}]}]",
        "{\"Type\":\"container\",\"Action\":\"start\"}",
    };
    static const char *const targets[] = { "GET /containers/json HTTP/1.1" };
    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    if (listener < 0 || strlen(path) >= sizeof(address.sun_path))
        return -1;
    snprintf(address.sun_path, sizeof(address.sun_path), "%s", path);
    unlink(path);
    mode_t old_mask = umask(0077);
    int bound = bind(listener, (struct sockaddr *)&address, sizeof(address));
    umask(old_mask);
    if (bound != 0 || chmod(path, 0600) != 0 || listen(listener, 3) != 0 ||
        fixture_write_all(ready_fd, "1", 1) != 0) {
        close(listener);
        unlink(path);
        return -1;
    }
    close(ready_fd);
    size_t requests = (size_t)cycles * 2;
    for (size_t request_index = 0; request_index < requests; request_index++) {
        size_t i = request_index % 2;
        int client = accept(listener, NULL, NULL);
        if (i == 0 && request_index / 2 + 1 == replace_at_cycle &&
            fixture_replace_master_socket(master_socket) != 0) {
            if (client >= 0)
                close(client);
            close(listener);
            unlink(path);
            return -1;
        }
        char request[512] = {0}, response[2048];
        ssize_t read_count = client < 0 ? -1 : read(client, request,
                                                     sizeof(request) - 1);
        int length = i == 0 ?
            snprintf(response, sizeof(response),
                     "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n"
                     "Connection: close\r\n\r\n%zx\r\n%s\r\n0\r\n\r\n",
                     strlen(bodies[i]), bodies[i]) :
            snprintf(response, sizeof(response),
                     "HTTP/1.1 200 OK\r\nContent-Length: %zu\r\n"
                     "Connection: close\r\n\r\n%s",
                     strlen(bodies[i]), bodies[i]);
        const char *expected_target = i == 1 ?
            strstr(request, "GET /events?since=") : strstr(request, targets[i]);
        int valid = read_count > 0 && expected_target &&
            length >= 0 && length < (int)sizeof(response) &&
            fixture_write_all(client, response, (size_t)length) == 0;
        if (client >= 0)
            close(client);
        if (!valid) {
            close(listener);
            unlink(path);
            return -1;
        }
    }
    close(listener);
    unlink(path);
    return 0;
}

static const char fixture_lease[] = "0123456789abcdef0123456789abcdef";

/* Runs the observer's watch loop for `cycles` passes against the fixture
 * Engine on PROFILE/docker.sock. Returns 0 when every pass was served. */
static int watch_fixture(struct profile *profile, const char *directory,
                         unsigned cycles, unsigned replace_at_cycle)
{
    char socket_path[PATH_MAX], lease_path[PATH_MAX], master_socket[PATH_MAX];
    memset(profile, 0, sizeof(*profile));
    if (!directory || cycles == 0 ||
        snprintf(profile->dir, sizeof(profile->dir), "%s", directory) >=
            (int)sizeof(profile->dir) ||
        snprintf(socket_path, sizeof(socket_path), "%s/docker.sock",
                 directory) >= (int)sizeof(socket_path) ||
        snprintf(lease_path, sizeof(lease_path), "%s/port-observer.lease",
                 directory) >= (int)sizeof(lease_path) ||
        snprintf(master_socket, sizeof(master_socket), "%s/ssh.sock",
                 directory) >= (int)sizeof(master_socket))
        return -1;
    int ready[2];
    if (pipe(ready) != 0)
        return -1;
    pid_t child = fork();
    if (child < 0)
        return -1;
    if (child == 0) {
        close(ready[0]);
        _exit(snapshot_fixture_server(socket_path, ready[1], cycles,
                                      master_socket, replace_at_cycle) == 0 ?
              0 : 1);
    }
    close(ready[1]);
    char marker = '\0';
    int ready_ok = read(ready[0], &marker, 1) == 1 && marker == '1';
    close(ready[0]);
    int watched = ready_ok && fs_write_file_atomic(lease_path,
        "0123456789abcdef0123456789abcdef\n", sizeof(fixture_lease),
        0600) == 0 &&
        docker_observer_watch(profile, "192.0.2.10", fixture_lease,
                              cycles) == 0;
    /* The server ends once it has served every pass. A pass that made fewer
     * requests leaves it waiting: give it 2 s, then end it as a failure. */
    int status = 0;
    pid_t waited = 0;
    for (int i = 0; watched && i < 200 && waited == 0; i++) {
        waited = waitpid(child, &status, WNOHANG);
        if (waited == 0)
            usleep(10 * 1000);
    }
    if (waited != child) {
        kill(child, SIGKILL);
        while (waitpid(child, &status, 0) < 0 && errno == EINTR)
            ;
        return -1;
    }
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
}

static int inspect_snapshot_fixture(const char *directory)
{
    struct profile profile;
    char state_path[PATH_MAX];
    if (!directory || snprintf(state_path, sizeof(state_path),
                               "%s/port-forwards.tsv", directory) >=
            (int)sizeof(state_path))
        return 1;
    int synchronized = watch_fixture(&profile, directory, 1, 0) == 0;
    char state[1024] = {0};
    FILE *f = fopen(state_path, "r");
    int state_ok = f && fread(state, 1, sizeof(state) - 1, f) > 0 &&
        fclose(f) == 0 && strstr(state, "tcp\t127.0.0.1\t48250\t80") &&
        strstr(state, "udp\t0.0.0.0\t48251\t53");
    int cleaned = port_forward_cleanup(&profile, "192.0.2.10") == 0 &&
        docker_observer_revoke(&profile) == 0 &&
        docker_observer_sync_once(&profile, "192.0.2.10",
                                  fixture_lease) == 1;
    return synchronized && state_ok && cleaned ? 0 : 1;
}

static int parse_count(const char *text, unsigned *count)
{
    char *end = NULL;
    errno = 0;
    unsigned long value = strtoul(text, &end, 10);
    if (errno || !end || end == text || *end || value > 64)
        return -1;
    *count = (unsigned)value;
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "list-fixtures") == 0)
        return list_parser_fixtures();
    if (argc == 3 && strcmp(argv[1], "read-snapshot") == 0) {
        struct profile p = {0};
        struct port_spec ports[DOCKER_OBSERVER_MAX_PORTS] = {0};
        int count = 0;
        if (snprintf(p.dir, sizeof(p.dir), "%s", argv[2]) >= (int)sizeof(p.dir)) return 1;
        if (docker_observer_read_snapshot(&p, ports, &count, DOCKER_OBSERVER_MAX_PORTS)) return 1;
        printf("%d\n", count);
        return 0;
    }
    if (argc > 1 && strcmp(argv[1], "udp-forward") == 0)
        return cmd_udp_forward(argc - 1, argv + 1);
    if (argc == 2 && strcmp(argv[1], "ignore-sigterm") == 0)
        return ignore_sigterm();
    if (argc == 2 && strcmp(argv[1], "spawn-ignore-sigterm") == 0)
        return spawn_ignore_sigterm();
    if (argc == 3 && strcmp(argv[1], "parse") == 0) {
        struct port_spec parsed;
        return load_spec(argv[2], &parsed) == 0 ? 0 : 2;
    }
    if (argc == 2 && strcmp(argv[1], "inspect-fixtures") == 0)
        return inspect_parser_fixtures();
    if (argc == 3 && strcmp(argv[1], "snapshot-fixture") == 0)
        return inspect_snapshot_fixture(argv[2]);
    if (argc == 5 && strcmp(argv[1], "observe-fixture") == 0) {
        /* CYCLES passes of one observer against the fixture Engine; the
         * master socket is replaced during pass REPLACE-AT (0: never). The
         * state stays for the caller to inspect and clean up. */
        struct profile profile;
        unsigned cycles = 0, replace_at_cycle = 0;
        if (parse_count(argv[3], &cycles) != 0 ||
            parse_count(argv[4], &replace_at_cycle) != 0)
            return 2;
        return watch_fixture(&profile, argv[2], cycles,
                             replace_at_cycle) == 0 ? 0 : 1;
    }

    const char *directory = getenv("PORT_TEST_DIR");
    if (!directory || argc < 2)
        return 2;
    struct profile profile = {0};
    snprintf(profile.dir, sizeof(profile.dir), "%s", directory);
    const char *guest_ip = "192.0.2.10";

    if (strcmp(argv[1], "cleanup") == 0)
        return port_forward_cleanup(&profile, guest_ip) == 0 ? 0 : 1;
    if (strcmp(argv[1], "failures") == 0 && argc == 2) {
        /* What VM status reports for a running VM. */
        cJSON *failures = port_forward_failures(&profile);
        char *text = failures ? cJSON_PrintUnformatted(failures) : NULL;
        cJSON_Delete(failures);
        if (!text)
            return 1;
        puts(text);
        cJSON_free(text);
        return 0;
    }
    if (strcmp(argv[1], "watch-unavailable") == 0 && argc == 2) {
        /* Three passes of the watch loop against a profile that has a lease
         * and no Docker socket. */
        static const char lease[] = "0123456789abcdef0123456789abcdef";
        char lease_path[PATH_MAX];
        return snprintf(lease_path, sizeof(lease_path),
                        "%s/port-observer.lease", directory) <
                   (int)sizeof(lease_path) &&
               fs_write_file_atomic(lease_path,
                                    "0123456789abcdef0123456789abcdef\n",
                                    sizeof(lease), 0600) == 0 &&
               docker_observer_watch(&profile, guest_ip, lease, 3) == 0 ?
               0 : 1;
    }
    if (strcmp(argv[1], "unconfirm") == 0 && argc == 2) {
        int operation_lock = port_forward_operation_lock(&profile);
        if (operation_lock < 0)
            return 1;
        int result = port_forward_unconfirm_tcp_serialized(&profile);
        port_forward_operation_unlock(operation_lock);
        return result == 0 ? 0 : 1;
    }
    /* `sync` is one pass. `sync-again` is two passes of this one process, as
     * the observer makes them: a relay that the first pass starts is this
     * process's child during the second. The suite acts at a rendezvous
     * after each pass, and the exit status is that of the second. */
    int passes = strcmp(argv[1], "sync") == 0 ? 1 :
        strcmp(argv[1], "sync-again") == 0 ? 2 : 0;
    if (passes) {
        /* One more than the synchronization accepts, so that the refusal of
         * an oversized snapshot is the product's and not this driver's. */
        struct port_spec specs[DOCKER_OBSERVER_MAX_PORTS + 1];
        int spec_count = argc - 2;
        if (spec_count > (int)(sizeof(specs) / sizeof(specs[0])))
            return 2;
        for (int i = 0; i < spec_count; i++) {
            if (load_spec(argv[i + 2], &specs[i]) != 0)
                return 2;
        }
        int result = -1;
        for (int pass = 0; pass < passes; pass++) {
            int operation_lock = port_forward_operation_lock(&profile);
            if (operation_lock < 0)
                return 1;
            result = port_forward_sync_docker_serialized(
                &profile, guest_ip, specs, spec_count);
            port_forward_operation_unlock(operation_lock);
            if (passes > 1 && suite_rendezvous() != 0)
                return 2;
        }
        return result == 0 ? 0 : 1;
    }
    return 2;
}
