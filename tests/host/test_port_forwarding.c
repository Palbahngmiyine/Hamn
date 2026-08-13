#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <limits.h>
#include <semaphore.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/proc_info.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

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

static int parent_holds_state_lock(const struct profile *p)
{
    pid_t child = fork();
    if (child < 0)
        return 0;
    if (child == 0) {
        char path[1200];
        snprintf(path, sizeof(path), "%s/port-forwards.lock", p->dir);
        int fd = open(path, O_RDWR | O_CLOEXEC);
        int held = fd >= 0 && flock(fd, LOCK_EX | LOCK_NB) != 0 &&
                   (errno == EWOULDBLOCK || errno == EAGAIN);
        if (fd >= 0)
            close(fd);
        _exit(held ? 0 : 1);
    }
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status) &&
           WEXITSTATUS(status) == 0;
}

static void unlocked_regression_barrier(const struct profile *p)
{
    const char *name = getenv("PORT_TEST_BARRIER_NAME");
    const char *counter_path = getenv("PORT_TEST_BARRIER_COUNTER");
    const char *expected_text = getenv("PORT_TEST_BARRIER_COUNT");
    if (!name || !counter_path || !expected_text ||
        parent_holds_state_lock(p))
        return;

    char *end = NULL;
    errno = 0;
    long expected = strtol(expected_text, &end, 10);
    if (errno || !end || *end || expected < 2 || expected > 128)
        exit(90);
    sem_t *barrier = sem_open(name, O_CREAT, 0600, 0);
    if (barrier == SEM_FAILED)
        exit(91);
    int fd = open(counter_path, O_RDWR | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0)
        exit(92);
    struct flock lock = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 0,
    };
    while (fcntl(fd, F_SETLKW, &lock) != 0) {
        if (errno != EINTR)
            exit(93);
    }
    long arrived = 0;
    char text[32] = {0};
    ssize_t length = pread(fd, text, sizeof(text) - 1, 0);
    if (length > 0)
        arrived = strtol(text, NULL, 10);
    arrived++;
    int written = snprintf(text, sizeof(text), "%ld\n", arrived);
    if (ftruncate(fd, 0) != 0 ||
        pwrite(fd, text, (size_t)written, 0) != written)
        exit(94);
    lock.l_type = F_UNLCK;
    if (fcntl(fd, F_SETLK, &lock) != 0)
        exit(95);
    close(fd);

    if (arrived == expected) {
        for (long i = 0; i < expected; i++) {
            if (sem_post(barrier) != 0)
                exit(96);
        }
        sem_unlink(name);
    }
    while (sem_wait(barrier) != 0) {
        if (errno != EINTR)
            exit(97);
    }
    sem_close(barrier);
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
    (void)ip;
    (void)remote_address;
    (void)remote_port;
    append_event("add", bind_address, local_port);
    unlocked_regression_barrier(p);
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
    (void)ip;
    (void)remote_address;
    (void)remote_port;
    append_event("cancel", bind_address, local_port);
    unlocked_regression_barrier(p);
    return configured_port("FAIL_CANCEL_PORT", local_port) ? -1 : 0;
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

static int load_generation(char **argv,
                           struct port_forward_generation *generation)
{
    char *end = NULL;
    errno = 0;
    long pid = strtol(argv[0], &end, 10);
    if (errno || !end || *end || pid <= 1 || pid > INT32_MAX)
        return -1;
    errno = 0;
    unsigned long long sec = strtoull(argv[1], &end, 10);
    if (errno || !end || *end || sec == 0 || sec > UINT64_MAX)
        return -1;
    errno = 0;
    unsigned long long usec = strtoull(argv[2], &end, 10);
    if (errno || !end || *end || usec >= 1000000 || usec > UINT64_MAX)
        return -1;
    generation->owner_pid = (int)pid;
    generation->owner_start_sec = (uint64_t)sec;
    generation->owner_start_usec = (uint64_t)usec;
    return 0;
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

static int print_process_token(const char *text)
{
    char *end = NULL;
    errno = 0;
    long pid = strtol(text, &end, 10);
    if (errno || !end || *end || pid <= 1 || pid > INT32_MAX)
        return 2;
    struct proc_bsdinfo info;
    int size = proc_pidinfo((int)pid, PROC_PIDTBSDINFO, 0, &info,
                            sizeof(info));
    if (size != (int)sizeof(info) || info.pbi_pid != (uint32_t)pid)
        return 1;
    printf("%llu\t%llu\n", (unsigned long long)info.pbi_start_tvsec,
           (unsigned long long)info.pbi_start_tvusec);
    return 0;
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

static int snapshot_fixture_server(const char *path, int ready_fd)
{
    static const char *const bodies[] = {
        ("[{\"Id\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"},"
         "{\"Id\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"}]"),
        "{\"NetworkSettings\":{\"Ports\":{\"80/tcp\":[{\"HostIp\":\"127.0.0.1\",\"HostPort\":\"48250\"}]}}}",
        "{\"NetworkSettings\":{\"Ports\":{\"53/udp\":[{\"HostIp\":\"0.0.0.0\",\"HostPort\":\"48251\"}]}}}",
        "{\"Type\":\"container\",\"Action\":\"start\"}",
    };
    static const char *const targets[] = {
        "GET /containers/json HTTP/1.1",
        "GET /containers/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/json HTTP/1.1",
        "GET /containers/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/json HTTP/1.1",
    };
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
    for (size_t i = 0; i < sizeof(bodies) / sizeof(bodies[0]); i++) {
        int client = accept(listener, NULL, NULL);
        char request[512] = {0}, response[2048];
        ssize_t read_count = client < 0 ? -1 : read(client, request,
                                                     sizeof(request) - 1);
        int length = i == 0 || i == 3 ?
            snprintf(response, sizeof(response),
                     "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n"
                     "Connection: close\r\n\r\n%zx\r\n%s\r\n0\r\n\r\n",
                     strlen(bodies[i]), bodies[i]) :
            snprintf(response, sizeof(response),
                     "HTTP/1.1 200 OK\r\nContent-Length: %zu\r\n"
                     "Connection: close\r\n\r\n%s",
                     strlen(bodies[i]), bodies[i]);
        const char *expected_target = i == 3 ?
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

static int inspect_snapshot_fixture(const char *directory)
{
    struct profile profile = {0};
    char socket_path[PATH_MAX], lease_path[PATH_MAX], state_path[PATH_MAX];
    static const char lease[] = "0123456789abcdef0123456789abcdef";
    if (!directory || snprintf(profile.dir, sizeof(profile.dir), "%s",
                               directory) >= (int)sizeof(profile.dir) ||
        snprintf(socket_path, sizeof(socket_path), "%s/docker.sock",
                 directory) >= (int)sizeof(socket_path) ||
        snprintf(lease_path, sizeof(lease_path), "%s/port-observer.lease",
                 directory) >= (int)sizeof(lease_path) ||
        snprintf(state_path, sizeof(state_path), "%s/port-forwards.tsv",
                 directory) >= (int)sizeof(state_path))
        return 1;
    int ready[2];
    if (pipe(ready) != 0)
        return 1;
    pid_t child = fork();
    if (child < 0)
        return 1;
    if (child == 0) {
        close(ready[0]);
        _exit(snapshot_fixture_server(socket_path, ready[1]) == 0 ? 0 : 1);
    }
    close(ready[1]);
    char marker = '\0';
    int ready_ok = read(ready[0], &marker, 1) == 1 && marker == '1';
    close(ready[0]);
    int synchronized = ready_ok && fs_write_file_atomic(lease_path,
        "0123456789abcdef0123456789abcdef\n", sizeof(lease), 0600) == 0 &&
        docker_observer_watch(&profile, "192.0.2.10", lease, 1) == 0;
    int status = 0;
    char state[1024] = {0};
    FILE *f = fopen(state_path, "r");
    int state_ok = f && fread(state, 1, sizeof(state) - 1, f) > 0 &&
        fclose(f) == 0 && strstr(state, "tcp\t127.0.0.1\t48250\t80") &&
        strstr(state, "udp\t0.0.0.0\t48251\t53");
    int cleaned = port_forward_cleanup(&profile, "192.0.2.10") == 0 &&
        docker_observer_revoke(&profile) == 0 &&
        docker_observer_sync_once(&profile, "192.0.2.10", lease) == 1;
    return synchronized && state_ok && cleaned &&
        waitpid(child, &status, 0) == child && WIFEXITED(status) &&
        WEXITSTATUS(status) == 0 ? 0 : 1;
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "udp-forward") == 0)
        return cmd_udp_forward(argc - 1, argv + 1);
    if (argc == 2 && strcmp(argv[1], "ignore-sigterm") == 0)
        return ignore_sigterm();
    if (argc == 2 && strcmp(argv[1], "spawn-ignore-sigterm") == 0)
        return spawn_ignore_sigterm();
    if (argc == 3 && strcmp(argv[1], "process-token") == 0)
        return print_process_token(argv[2]);
    if (argc == 3 && strcmp(argv[1], "parse") == 0) {
        struct port_spec parsed;
        return load_spec(argv[2], &parsed) == 0 ? 0 : 2;
    }
    if (argc == 2 && strcmp(argv[1], "inspect-fixtures") == 0)
        return inspect_parser_fixtures();
    if (argc == 3 && strcmp(argv[1], "snapshot-fixture") == 0)
        return inspect_snapshot_fixture(argv[2]);

    const char *directory = getenv("PORT_TEST_DIR");
    if (!directory || argc < 2)
        return 2;
    struct profile profile = {0};
    snprintf(profile.dir, sizeof(profile.dir), "%s", directory);
    const char *guest_ip = "192.0.2.10";

    if (strcmp(argv[1], "cleanup") == 0)
        return port_forward_cleanup(&profile, guest_ip) == 0 ? 0 : 1;
    if (strcmp(argv[1], "reconcile") == 0 && argc == 3)
        return port_forward_reconcile(&profile, guest_ip, argv[2]) == 0 ?
               0 : 1;
    if (strcmp(argv[1], "reconcile-serialized") == 0 && argc == 3) {
        int operation_lock = port_forward_operation_lock(&profile);
        if (operation_lock < 0)
            return 1;
        int result = port_forward_reconcile_serialized(&profile, guest_ip,
                                                       argv[2]);
        port_forward_operation_unlock(operation_lock);
        return result == 0 ? 0 : 1;
    }
    if (strcmp(argv[1], "sync") == 0) {
        struct port_spec specs[128];
        int spec_count = argc - 2;
        if (spec_count > (int)(sizeof(specs) / sizeof(specs[0])))
            return 2;
        for (int i = 0; i < spec_count; i++) {
            if (load_spec(argv[i + 2], &specs[i]) != 0)
                return 2;
        }
        int operation_lock = port_forward_operation_lock(&profile);
        if (operation_lock < 0)
            return 1;
        int result = port_forward_sync_docker_serialized(
            &profile, guest_ip, specs, spec_count);
        port_forward_operation_unlock(operation_lock);
        return result == 0 ? 0 : 1;
    }
    if (argc != 3 && argc != 6)
        return 2;

    struct port_spec spec;
    if (load_spec(argv[2], &spec) != 0)
        return 2;
    if (argc == 6) {
        struct port_forward_generation generation;
        if (load_generation(argv + 3, &generation) != 0)
            return 2;
        if (strcmp(argv[1], "commit-owned") == 0)
            return port_forward_commit_owned(&profile, &spec,
                                             &generation) == 0 ? 0 : 1;
        if (strcmp(argv[1], "remove-owned") == 0)
            return port_forward_remove_owned(&profile, guest_ip, &spec,
                                             &generation) == 0 ? 0 : 1;
        return 2;
    }
    if (strcmp(argv[1], "add") == 0)
        return port_forward_add(&profile, guest_ip, &spec) == 0 ? 0 : 1;
    if (strcmp(argv[1], "commit") == 0)
        return port_forward_commit(&profile, &spec) == 0 ? 0 : 1;
    if (strcmp(argv[1], "remove") == 0)
        return port_forward_remove(&profile, guest_ip, &spec) == 0 ? 0 : 1;
    return 2;
}
