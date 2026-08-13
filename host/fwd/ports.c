#include "fwd/ports.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <libproc.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/proc_info.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#include "core/log.h"
#include "sshmgr/ssh.h"
#include "util/fs.h"
#include "util/proc.h"

#define MAX_FORWARD_RECORDS 128

struct forward_record {
    struct port_spec spec;
    int pid;
    uint64_t start_sec;
    uint64_t start_usec;
    int pending;
    int submitted;
    int serialized;
    int owner_pid;
    uint64_t owner_start_sec;
    uint64_t owner_start_usec;
};

static const char *protocol_name(enum port_protocol protocol)
{
    return protocol == PORT_UDP ? "udp" : "tcp";
}

int port_number_parse(const char *text, unsigned *port)
{
    char *end = NULL;
    errno = 0;
    unsigned long value = strtoul(text, &end, 10);
    if (errno || !end || *end || value == 0 || value > 65535)
        return -1;
    *port = (unsigned)value;
    return 0;
}

static int set_error(char *error, size_t cap, const char *text)
{
    if (error && cap)
        snprintf(error, cap, "%s", text);
    return -1;
}

int port_spec_parse(const char *text, struct port_spec *spec,
                    char *error, size_t error_cap)
{
    if (!text || strlen(text) >= 256)
        return set_error(error, error_cap, "port specification is too long");
    char copy[256];
    snprintf(copy, sizeof(copy), "%s", text);
    memset(spec, 0, sizeof(*spec));
    snprintf(spec->host_ip, sizeof(spec->host_ip), "127.0.0.1");
    spec->protocol = PORT_TCP;

    char *slash = strrchr(copy, '/');
    if (slash) {
        *slash++ = '\0';
        if (strcmp(slash, "tcp") == 0)
            spec->protocol = PORT_TCP;
        else if (strcmp(slash, "udp") == 0)
            spec->protocol = PORT_UDP;
        else
            return set_error(error, error_cap,
                             "port protocol must be tcp or udp");
    }

    char *parts[3] = {0};
    int count = 0;
    char *save = NULL;
    for (char *part = strtok_r(copy, ":", &save); part;
         part = strtok_r(NULL, ":", &save)) {
        if (count == 3)
            return set_error(error, error_cap,
                             "IPv6 and port ranges are not supported");
        parts[count++] = part;
    }
    if (count == 1)
        return set_error(error, error_cap,
                         "an explicit host port is required");
    if (count == 2) {
        if (port_number_parse(parts[0], &spec->host_port) != 0 ||
            port_number_parse(parts[1], &spec->container_port) != 0)
            return set_error(error, error_cap, "invalid port number");
    } else if (count == 3) {
        struct in_addr address;
        if (inet_pton(AF_INET, parts[0], &address) != 1)
            return set_error(error, error_cap,
                             "host bind address must be IPv4");
        snprintf(spec->host_ip, sizeof(spec->host_ip), "%s", parts[0]);
        if (port_number_parse(parts[1], &spec->host_port) != 0 ||
            port_number_parse(parts[2], &spec->container_port) != 0)
            return set_error(error, error_cap, "invalid port number");
    } else {
        return set_error(error, error_cap, "invalid port specification");
    }
    return 0;
}

int port_spec_guest_text(const struct port_spec *spec, const char *guest_ip,
                         char *text, size_t cap)
{
    const char *bind_ip = spec->protocol == PORT_UDP ? guest_ip : "127.0.0.1";
    int n = snprintf(text, cap, "%s:%u:%u/%s", bind_ip, spec->host_port,
                     spec->container_port, protocol_name(spec->protocol));
    return n >= 0 && n < (int)cap ? 0 : -1;
}

static int ownership_parse(const char *ownership,
                           struct forward_record *record)
{
    if (strcmp(ownership, "pending") == 0) {
        record->pending = 1;
    } else if (strcmp(ownership, "control-locked") == 0) {
        record->pending = 1;
        record->serialized = 1;
    } else if (strcmp(ownership, "submitted") == 0 ||
               strcmp(ownership, "submitted-locked") == 0) {
        record->pending = 1;
        record->submitted = 1;
        record->serialized = strcmp(ownership, "submitted-locked") == 0;
    } else if (strcmp(ownership, "committed") != 0) {
        return -1;
    }
    return 0;
}

static int records_load(const struct profile *p,
                        struct forward_record records[], int *count)
{
    *count = 0;
    char path[1100];
    profile_path(p, "port-forwards.tsv", path, sizeof(path));
    FILE *f = fopen(path, "r");
    if (!f)
        return errno == ENOENT ? 0 : -1;
    char line[320], protocol[8], ownership[24];
    while (fgets(line, sizeof(line), f)) {
        struct forward_record record = {0};
        int consumed = 0;
        int parsed = sscanf(line,
                            "%7s\t%63s\t%u\t%u\t%d\t%" SCNu64
                            "\t%" SCNu64 "\t%23s\t%d\t%" SCNu64
                            "\t%" SCNu64 "%n",
                            protocol, record.spec.host_ip,
                            &record.spec.host_port,
                            &record.spec.container_port, &record.pid,
                            &record.start_sec, &record.start_usec, ownership,
                            &record.owner_pid, &record.owner_start_sec,
                            &record.owner_start_usec,
                            &consumed);
        if (parsed != 11) {
            record.owner_pid = 0;
            record.owner_start_sec = 0;
            record.owner_start_usec = 0;
            parsed = sscanf(line,
                            "%7s\t%63s\t%u\t%u\t%d\t%" SCNu64
                            "\t%" SCNu64 "\t%23s%n",
                            protocol, record.spec.host_ip,
                            &record.spec.host_port,
                            &record.spec.container_port, &record.pid,
                            &record.start_sec, &record.start_usec, ownership,
                            &consumed);
        }
        if ((parsed == 8 || parsed == 11) &&
            ownership_parse(ownership, &record) != 0) {
            fclose(f);
            return -1;
        }
        if (parsed != 8 && parsed != 11) {
            record.start_sec = 0;
            record.start_usec = 0;
            parsed = sscanf(line,
                            "%7s\t%63s\t%u\t%u\t%d\t%" SCNu64
                            "\t%" SCNu64 "%n",
                            protocol, record.spec.host_ip,
                            &record.spec.host_port,
                            &record.spec.container_port, &record.pid,
                            &record.start_sec, &record.start_usec, &consumed);
            if (parsed != 7) {
                record.start_sec = 0;
                record.start_usec = 0;
                parsed = sscanf(line, "%7s\t%63s\t%u\t%u\t%d%n", protocol,
                                record.spec.host_ip, &record.spec.host_port,
                                &record.spec.container_port, &record.pid,
                                &consumed);
            }
        }
        if (parsed != 5 && parsed != 7 && parsed != 8 && parsed != 11) {
            fclose(f);
            return -1;
        }
        while (line[consumed] == '\r' || line[consumed] == '\n')
            consumed++;
        struct in_addr address;
        if (line[consumed] != '\0' ||
            (strcmp(protocol, "tcp") != 0 && strcmp(protocol, "udp") != 0) ||
            inet_pton(AF_INET, record.spec.host_ip, &address) != 1 ||
            record.spec.host_port == 0 || record.spec.host_port > 65535 ||
            record.spec.container_port == 0 ||
            record.spec.container_port > 65535 || record.pid < 0 ||
            record.start_usec >= 1000000 ||
            (record.start_sec == 0 && record.start_usec != 0) ||
            record.owner_pid < 0 || record.owner_start_usec >= 1000000 ||
            (record.owner_pid != 0 && record.owner_pid <= 1) ||
            (record.owner_start_sec == 0 && record.owner_start_usec != 0) ||
            ((record.owner_pid > 1) != (record.owner_start_sec > 0)) ||
            (record.submitted && !record.pending) ||
            (record.serialized && !record.pending) ||
            (!record.pending &&
             (record.owner_pid != 0 || record.owner_start_sec != 0 ||
              record.owner_start_usec != 0)) ||
            *count >= MAX_FORWARD_RECORDS) {
            fclose(f);
            return -1;
        }
        record.spec.protocol = strcmp(protocol, "udp") == 0 ?
                               PORT_UDP : PORT_TCP;
        records[(*count)++] = record;
    }
    int rc = ferror(f) ? -1 : 0;
    fclose(f);
    return rc;
}

static int records_save(const struct profile *p,
                        const struct forward_record records[], int count)
{
    char text[MAX_FORWARD_RECORDS * 224];
    size_t off = 0;
    for (int i = 0; i < count; i++) {
        int n = snprintf(text + off, sizeof(text) - off,
                         "%s\t%s\t%u\t%u\t%d\t%" PRIu64
                         "\t%" PRIu64 "\t%s\t%d\t%" PRIu64
                         "\t%" PRIu64 "\n",
                         protocol_name(records[i].spec.protocol),
                         records[i].spec.host_ip,
                         records[i].spec.host_port,
                         records[i].spec.container_port, records[i].pid,
                         records[i].start_sec, records[i].start_usec,
                         records[i].serialized && records[i].submitted ?
                         "submitted-locked" :
                         records[i].serialized ? "control-locked" :
                         records[i].submitted ? "submitted" :
                         records[i].pending ? "pending" : "committed",
                         records[i].owner_pid, records[i].owner_start_sec,
                         records[i].owner_start_usec);
        if (n < 0 || n >= (int)(sizeof(text) - off))
            return -1;
        off += (size_t)n;
    }
    char path[1100];
    profile_path(p, "port-forwards.tsv", path, sizeof(path));
    if (count == 0)
        return fs_unlink_if_exists(path);
    return fs_write_file_atomic(path, text, off, 0600);
}

static int state_lock(const struct profile *p)
{
    char path[1100];
    profile_path(p, "port-forwards.lock", path, sizeof(path));
    int fd = open(path, O_RDWR | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0)
        return -1;
    while (flock(fd, LOCK_EX) != 0) {
        if (errno == EINTR)
            continue;
        close(fd);
        return -1;
    }
    return fd;
}

int port_forward_operation_lock(const struct profile *p)
{
    char path[1100];
    profile_path(p, "port-forward-operations.lock", path, sizeof(path));
    int fd = open(path, O_RDWR | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0)
        return -1;
    while (flock(fd, LOCK_EX) != 0) {
        if (errno == EINTR)
            continue;
        close(fd);
        return -1;
    }
    return fd;
}

void port_forward_operation_unlock(int lock_fd)
{
    if (lock_fd < 0)
        return;
    /*
     * A forked supervisor shares this flock's open file description. Closing
     * our reference keeps the operation serialized until the last supervisor
     * reference closes; an explicit LOCK_UN would release it process-wide.
     */
    close(lock_fd);
}

static int process_start_token(int pid, uint64_t *start_sec,
                               uint64_t *start_usec)
{
    struct proc_bsdinfo info;
    int size = proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info));
    if (size != (int)sizeof(info) || info.pbi_pid != (uint32_t)pid)
        return -1;
    *start_sec = info.pbi_start_tvsec;
    *start_usec = info.pbi_start_tvusec;
    return 0;
}

int port_forward_generation_current(struct port_forward_generation *generation)
{
    if (!generation)
        return -1;
    memset(generation, 0, sizeof(*generation));
    generation->owner_pid = (int)getpid();
    if (generation->owner_pid <= 1 ||
        process_start_token(generation->owner_pid,
                            &generation->owner_start_sec,
                            &generation->owner_start_usec) != 0) {
        memset(generation, 0, sizeof(*generation));
        return -1;
    }
    return 0;
}

static int generation_valid(const struct port_forward_generation *generation)
{
    return generation && generation->owner_pid > 1 &&
           generation->owner_start_sec > 0 &&
           generation->owner_start_usec < 1000000;
}

static int generation_matches(
    const struct forward_record *record,
    const struct port_forward_generation *generation)
{
    return generation_valid(generation) && record->pending &&
           record->owner_pid == generation->owner_pid &&
           record->owner_start_sec == generation->owner_start_sec &&
           record->owner_start_usec == generation->owner_start_usec;
}

enum process_identity {
    PROCESS_IDENTITY_UNKNOWN = -1,
    PROCESS_IDENTITY_CHANGED = 0,
    PROCESS_IDENTITY_MATCH = 1,
};

static enum process_identity process_identity_values(int pid,
                                                      uint64_t start_sec,
                                                      uint64_t start_usec)
{
    /* A PID alone is unsafe after reuse; both start-time fields must match. */
    if (pid <= 1 || start_sec == 0)
        return PROCESS_IDENTITY_UNKNOWN;
    uint64_t actual_sec = 0, actual_usec = 0;
    if (process_start_token(pid, &actual_sec, &actual_usec) == 0) {
        return actual_sec == start_sec && actual_usec == start_usec ?
               PROCESS_IDENTITY_MATCH : PROCESS_IDENTITY_CHANGED;
    }
    if (kill(pid, 0) != 0 && errno == ESRCH)
        return PROCESS_IDENTITY_CHANGED;
    return PROCESS_IDENTITY_UNKNOWN;
}

static enum process_identity process_identity(const struct forward_record *record)
{
    return process_identity_values(record->pid, record->start_sec,
                                   record->start_usec);
}

static enum process_identity owner_identity(const struct forward_record *record)
{
    return process_identity_values(record->owner_pid,
                                   record->owner_start_sec,
                                   record->owner_start_usec);
}

static int same_listener(const struct port_spec *a, const struct port_spec *b)
{
    /* Every macOS address maps to one guest protocol/port listener. */
    return a->protocol == b->protocol && a->host_port == b->host_port;
}

static int same_forward(const struct port_spec *a, const struct port_spec *b)
{
    return a->protocol == b->protocol &&
           strcmp(a->host_ip, b->host_ip) == 0 &&
           a->host_port == b->host_port &&
           a->container_port == b->container_port;
}

static int udp_pidfile(const struct profile *p, const struct port_spec *spec,
                       char *path, size_t cap)
{
    char file[128], safe_ip[64];
    snprintf(safe_ip, sizeof(safe_ip), "%s", spec->host_ip);
    for (char *c = safe_ip; *c; c++) {
        if (*c == '.')
            *c = '-';
    }
    int n = snprintf(file, sizeof(file), "udp-%s-%u.pid", safe_ip,
                     spec->host_port);
    if (n < 0 || n >= (int)sizeof(file))
        return -1;
    profile_path(p, file, path, cap);
    return 0;
}

static int udp_pidfile_identity(const char *path, int *pid,
                                uint64_t *start_sec, uint64_t *start_usec)
{
    FILE *f = fopen(path, "r");
    if (!f)
        return -1;
    char line[128];
    if (!fgets(line, sizeof(line), f) || ferror(f)) {
        fclose(f);
        errno = EINVAL;
        return -1;
    }
    int extra = fgetc(f);
    fclose(f);
    if (extra != EOF) {
        errno = EINVAL;
        return -1;
    }

    int consumed = 0;
    if (sscanf(line, "%d\t%" SCNu64 "\t%" SCNu64 "%n", pid,
               start_sec, start_usec, &consumed) != 3) {
        errno = EINVAL;
        return -1;
    }
    while (line[consumed] == '\r' || line[consumed] == '\n')
        consumed++;
    if (line[consumed] != '\0' || *pid <= 1 || *start_sec == 0 ||
        *start_usec >= 1000000) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

static int prepare_udp_pidfile(const struct profile *p,
                               const struct port_spec *spec)
{
    char pidfile[1100];
    int pid = 0;
    uint64_t start_sec = 0, start_usec = 0;
    if (udp_pidfile(p, spec, pidfile, sizeof(pidfile)) != 0)
        return -1;
    if (udp_pidfile_identity(pidfile, &pid, &start_sec, &start_usec) != 0) {
        if (errno == ENOENT)
            return 0;
        logerr("refusing to replace unverified UDP pidfile for %s:%u",
               spec->host_ip, spec->host_port);
        return -1;
    }
    enum process_identity identity = process_identity_values(pid, start_sec,
                                                              start_usec);
    if (identity != PROCESS_IDENTITY_CHANGED) {
        logerr("refusing to replace live or unverified UDP pidfile for %s:%u",
               spec->host_ip, spec->host_port);
        return -1;
    }
    return fs_unlink_if_exists(pidfile);
}

static int open_udp_listener(const struct port_spec *spec)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return -1;
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_port = htons((uint16_t)spec->host_port),
    };
    if (inet_pton(AF_INET, spec->host_ip, &address.sin_addr) != 1 ||
        bind(fd, (struct sockaddr *)&address, sizeof(address)) != 0) {
        close(fd);
        return -1;
    }
    if (fd <= STDERR_FILENO) {
        int replacement = fcntl(fd, F_DUPFD, STDERR_FILENO + 1);
        close(fd);
        fd = replacement;
    }
    return fd;
}

static int wait_udp_started(const char *pidfile, int expected_pid,
                            struct forward_record *record)
{
    for (int i = 0; i < 40; i++) {
        int value = 0;
        uint64_t start_sec = 0, start_usec = 0;
        if (udp_pidfile_identity(pidfile, &value, &start_sec, &start_usec) ==
                0 &&
            value == expected_pid &&
            process_identity_values(value, start_sec, start_usec) ==
                PROCESS_IDENTITY_MATCH) {
            record->pid = value;
            record->start_sec = start_sec;
            record->start_usec = start_usec;
            return 0;
        }
        usleep(50 * 1000);
    }
    return -1;
}

static int wait_udp_stopped(const struct forward_record *record);

static int terminate_udp_process(const struct forward_record *record)
{
    enum process_identity identity = process_identity(record);
    if (identity == PROCESS_IDENTITY_CHANGED)
        return 0;
    if (identity != PROCESS_IDENTITY_MATCH)
        return -1;
    if (kill(record->pid, SIGTERM) != 0 &&
        process_identity(record) != PROCESS_IDENTITY_CHANGED)
        return -1;
    if (wait_udp_stopped(record) == 0)
        return 0;
    if (process_identity(record) != PROCESS_IDENTITY_MATCH)
        return -1;
    if (kill(record->pid, SIGKILL) != 0 &&
        process_identity(record) != PROCESS_IDENTITY_CHANGED)
        return -1;
    return wait_udp_stopped(record);
}

static int wait_spawned_child(pid_t pid)
{
    for (int i = 0; i < 20; i++) {
        int status = 0;
        pid_t waited;
        do {
            waited = waitpid(pid, &status, WNOHANG);
        } while (waited < 0 && errno == EINTR);
        if (waited == pid)
            return 0;
        if (waited < 0)
            return errno == ECHILD && kill(pid, 0) != 0 && errno == ESRCH ?
                   0 : -1;
        usleep(50 * 1000);
    }
    return -1;
}

static int terminate_spawned_child(pid_t pid)
{
    if (kill(pid, SIGTERM) != 0 && errno != ESRCH)
        return -1;
    if (wait_spawned_child(pid) == 0)
        return 0;
    if (kill(pid, SIGKILL) != 0 && errno != ESRCH)
        return -1;
    return wait_spawned_child(pid);
}

enum udp_start_result {
    UDP_START_UNSAFE = -2,
    UDP_START_FAILED = -1,
    UDP_START_OK = 0,
};

static int start_udp(const struct profile *p, const char *guest_ip,
                     const struct port_spec *spec,
                     struct forward_record *record)
{
    char self[PATH_MAX], pidfile[1100], logfile[1100], logdir[1100];
    char listen_port[16], target_port[16], listen_fd[16];
    if (!proc_self_path(self, sizeof(self)) ||
        udp_pidfile(p, spec, pidfile, sizeof(pidfile)) != 0 ||
        prepare_udp_pidfile(p, spec) != 0)
        return -1;
    profile_path(p, "logs", logdir, sizeof(logdir));
    if (fs_mkdirs(logdir, 0755) != 0)
        return -1;
    profile_path(p, "logs/udp-forward.log", logfile, sizeof(logfile));
    snprintf(listen_port, sizeof(listen_port), "%u", spec->host_port);
    snprintf(target_port, sizeof(target_port), "%u", spec->host_port);
    int listener = open_udp_listener(spec);
    if (listener < 0)
        return -1;
    snprintf(listen_fd, sizeof(listen_fd), "%d", listener);
    const char *argv[] = {
        self, "udp-forward",
        "--listen-address", spec->host_ip,
        "--listen-port", listen_port,
        "--listen-fd", listen_fd,
        "--target-address", guest_ip,
        "--target-port", target_port,
        "--pidfile", pidfile,
        NULL,
    };
    int spawned = (int)proc_spawn_daemon(argv, logfile);
    close(listener);
    if (spawned < 0) {
        logerr("cannot spawn UDP forward: %s", strerror(errno));
        return UDP_START_FAILED;
    }

    struct forward_record child = *record;
    if (process_start_token(spawned, &child.start_sec,
                            &child.start_usec) == 0)
        child.pid = spawned;
    *record = child;
    if (wait_udp_started(pidfile, spawned, record) != 0) {
        if (terminate_spawned_child(spawned) != 0) {
            *record = child;
            return UDP_START_UNSAFE;
        }
        unlink(pidfile);
        return UDP_START_FAILED;
    }
    return UDP_START_OK;
}

static void test_fifo_barrier(const char *ready_name, const char *release_name,
                              const char *description)
{
    const char *ready = getenv(ready_name);
    const char *release = getenv(release_name);
    if (!ready && !release)
        return;
    if (!ready || !release) {
        logerr("incomplete %s test barrier", description);
        return;
    }
    int fd = open(ready, O_WRONLY | O_CLOEXEC);
    if (fd < 0 || write(fd, "ready\n", 6) != 6) {
        if (fd >= 0)
            close(fd);
        logerr("cannot signal %s test barrier", description);
        return;
    }
    close(fd);
    fd = open(release, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        logerr("cannot wait at %s test barrier", description);
        return;
    }
    char byte;
    while (read(fd, &byte, 1) < 0 && errno == EINTR)
        ;
    close(fd);
}

static void udp_reservation_test_barrier(void)
{
    test_fifo_barrier("HAMN_TEST_UDP_RESERVATION_READY_FIFO",
                      "HAMN_TEST_UDP_RESERVATION_RELEASE_FIFO",
                      "UDP reservation");
}

static void tcp_reservation_test_barrier(void)
{
    test_fifo_barrier("HAMN_TEST_TCP_RESERVATION_READY_FIFO",
                      "HAMN_TEST_TCP_RESERVATION_RELEASE_FIFO",
                      "TCP reservation");
}

static void tcp_added_test_barrier(void)
{
    test_fifo_barrier("HAMN_TEST_TCP_ADDED_READY_FIFO",
                      "HAMN_TEST_TCP_ADDED_RELEASE_FIFO",
                      "TCP added state");
}

static void tcp_cancelled_test_barrier(void)
{
    test_fifo_barrier("HAMN_TEST_TCP_CANCELLED_READY_FIFO",
                      "HAMN_TEST_TCP_CANCELLED_RELEASE_FIFO",
                      "TCP cancelled state");
}

static void udp_state_test_barrier(void)
{
    test_fifo_barrier("HAMN_TEST_UDP_STATE_READY_FIFO",
                      "HAMN_TEST_UDP_STATE_RELEASE_FIFO", "UDP state");
}

static int wait_udp_stopped(const struct forward_record *record)
{
    for (int i = 0; i < 20; i++) {
        int status = 0;
        pid_t waited;
        do {
            waited = waitpid(record->pid, &status, WNOHANG);
        } while (waited < 0 && errno == EINTR);
        if (waited == record->pid)
            return 0;
        if (waited < 0 && errno != ECHILD)
            return -1;
        enum process_identity identity = process_identity(record);
        if (identity == PROCESS_IDENTITY_CHANGED)
            return 0;
        usleep(50 * 1000);
    }
    return process_identity(record) == PROCESS_IDENTITY_CHANGED ? 0 : -1;
}

static int remove_udp_pidfile(const struct profile *p,
                              const struct forward_record *record)
{
    char pidfile[1100];
    if (udp_pidfile(p, &record->spec, pidfile, sizeof(pidfile)) != 0)
        return -1;
    return fs_unlink_if_exists(pidfile);
}

enum udp_pending_recovery {
    UDP_PENDING_UNKNOWN = -1,
    UDP_PENDING_GONE = 0,
    UDP_PENDING_MATCH = 1,
};

static int udp_listener_available(const struct port_spec *spec)
{
    int fd = open_udp_listener(spec);
    if (fd < 0)
        return 0;
    close(fd);
    return 1;
}

static int tcp_listener_available(const struct port_spec *spec)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0)
        return 0;
    int reuse = 1;
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_port = htons((uint16_t)spec->host_port),
    };
    int available = setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse,
                               sizeof(reuse)) == 0 &&
        inet_pton(AF_INET, spec->host_ip, &address.sin_addr) == 1 &&
        bind(fd, (struct sockaddr *)&address, sizeof(address)) == 0;
    close(fd);
    return available;
}

static enum udp_pending_recovery recover_pending_udp(
    const struct profile *p, struct forward_record *record)
{
    char pidfile[1100];
    int pid = 0;
    uint64_t start_sec = 0, start_usec = 0;
    if (udp_pidfile(p, &record->spec, pidfile, sizeof(pidfile)) != 0)
        return UDP_PENDING_UNKNOWN;
    if (udp_pidfile_identity(pidfile, &pid, &start_sec, &start_usec) != 0) {
        if (errno != ENOENT)
            return UDP_PENDING_UNKNOWN;
        return udp_listener_available(&record->spec) ?
               UDP_PENDING_GONE : UDP_PENDING_UNKNOWN;
    }
    enum process_identity identity = process_identity_values(pid, start_sec,
                                                              start_usec);
    if (identity == PROCESS_IDENTITY_UNKNOWN)
        return UDP_PENDING_UNKNOWN;
    if (identity == PROCESS_IDENTITY_CHANGED) {
        if (remove_udp_pidfile(p, record) != 0)
            return UDP_PENDING_UNKNOWN;
        return UDP_PENDING_GONE;
    }
    record->pid = pid;
    record->start_sec = start_sec;
    record->start_usec = start_usec;
    return UDP_PENDING_MATCH;
}

static int stop_record(const struct profile *p, const char *guest_ip,
                       const struct forward_record *record)
{
    if (record->spec.protocol == PORT_TCP) {
        int cancelled = ssh_forward_cancel_tcp(
            p, guest_ip, record->spec.host_ip, record->spec.host_port,
            "127.0.0.1", record->spec.host_port) == 0;
        if (!cancelled && ssh_master_alive(p) == 0 &&
            !tcp_listener_available(&record->spec)) {
            logerr("cannot stop TCP forward on %s:%u",
                   record->spec.host_ip, record->spec.host_port);
            return -1;
        }
        return 0;
    }

    enum process_identity identity = process_identity(record);
    if (identity == PROCESS_IDENTITY_UNKNOWN) {
        logerr("refusing to stop unverified UDP forward process %d",
               record->pid);
        return -1;
    }
    if (identity == PROCESS_IDENTITY_MATCH &&
        terminate_udp_process(record) != 0) {
        logerr("cannot stop UDP forward process %d", record->pid);
        return -1;
    }
    if (remove_udp_pidfile(p, record) != 0) {
        logerr("cannot remove UDP forward pidfile for %d", record->pid);
        return -1;
    }
    return 0;
}

struct tcp_control_completion {
    const struct profile *profile;
    struct port_spec spec;
    int owner_pid;
    uint64_t owner_start_sec;
    uint64_t owner_start_usec;
};

static int complete_tcp_control(int rc, void *opaque)
{
    if (rc != 0)
        return 0;
    struct tcp_control_completion *completion = opaque;
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    if (!completion || records_load(completion->profile, records, &count) != 0)
        return -1;
    for (int i = 0; i < count; i++) {
        if (!same_forward(&records[i].spec, &completion->spec) ||
            !records[i].pending || records[i].submitted ||
            !records[i].serialized ||
            records[i].owner_pid != completion->owner_pid ||
            records[i].owner_start_sec != completion->owner_start_sec ||
            records[i].owner_start_usec != completion->owner_start_usec)
            continue;
        records[i].serialized = 0;
        return records_save(completion->profile, records, count);
    }
    return -1;
}

static int port_forward_add_under_operation_lock(const struct profile *p,
                                                 const char *guest_ip,
                                                 const struct port_spec *spec)
{
    int lock_fd = state_lock(p);
    if (lock_fd < 0) {
        logerr("cannot lock port forward state");
        return -1;
    }
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    int result = -1;
    if (records_load(p, records, &count) != 0) {
        logerr("cannot read port forward state");
        goto out;
    }
    for (int i = 0; i < count; i++) {
        if (same_listener(&records[i].spec, spec)) {
            logerr("host %s port %s:%u is already published",
                   protocol_name(spec->protocol), spec->host_ip,
                   spec->host_port);
            goto out;
        }
    }
    if (count >= MAX_FORWARD_RECORDS)
        goto out;

    /*
     * Persist ownership before creating the listener. If the filesystem
     * cannot reserve recovery state, no host resource is created.
     */
    struct forward_record record = {
        .spec = *spec,
        .pid = 0,
        .pending = 1,
        .owner_pid = (int)getpid(),
    };
    if (process_start_token(record.owner_pid, &record.owner_start_sec,
                            &record.owner_start_usec) != 0) {
        logerr("cannot identify port forward owner process");
        goto out;
    }
    records[count++] = record;
    if (records_save(p, records, count) != 0)
        goto out;

    if (spec->protocol == PORT_UDP)
        udp_reservation_test_barrier();
    else
        tcp_reservation_test_barrier();

    int rc;
    if (spec->protocol == PORT_TCP) {
        /*
         * A killed wrapper or failed SSH response may leave the master request
         * applied after our caller disappears. Persist an uncertain external
         * mutation phase before sending the control request; only explicit
         * cleanup or positive inventory may resolve it.
         */
        record.submitted = 0;
        record.serialized = 1;
        records[count - 1] = record;
        if (records_save(p, records, count) != 0)
            goto out;
        struct tcp_control_completion completion = {
            .profile = p,
            .spec = *spec,
            .owner_pid = record.owner_pid,
            .owner_start_sec = record.owner_start_sec,
            .owner_start_usec = record.owner_start_usec,
        };
        rc = ssh_forward_add_tcp_observed(
            p, guest_ip, spec->host_ip, spec->host_port, "127.0.0.1",
            spec->host_port, complete_tcp_control, &completion);
    } else {
        rc = start_udp(p, guest_ip, spec, &record);
        if (rc == 0)
            udp_state_test_barrier();
    }
    if (rc != 0) {
        logerr("cannot bind host %s port %s:%u", protocol_name(spec->protocol),
               spec->host_ip, spec->host_port);
        if (spec->protocol == PORT_TCP) {
            int cancelled = ssh_forward_cancel_tcp(
                p, guest_ip, spec->host_ip, spec->host_port, "127.0.0.1",
                spec->host_port) == 0;
            int absent = cancelled ||
                (ssh_master_alive(p) != 0 && tcp_listener_available(spec));
            if (absent) {
                if (records_save(p, records, count - 1) != 0)
                    logerr("cannot remove failed TCP forward state");
            } else {
                records[count - 1] = record;
                if (records_save(p, records, count) != 0)
                    logerr("cannot persist uncertain TCP forward state");
            }
        } else if (rc == UDP_START_UNSAFE) {
            records[count - 1] = record;
            if (records_save(p, records, count) != 0)
                logerr("cannot persist uncertain UDP forward identity");
        } else if (records_save(p, records, count - 1) != 0) {
            logerr("cannot remove reserved port forward state");
        }
        goto out;
    }

    if (spec->protocol == PORT_TCP) {
        /*
         * The exact SSH control request completed successfully. Return to the
         * recoverable host-listener-only phase before releasing either lock;
         * The Docker API observer marks remote publication in a separate
         * durable step.
         */
        record.submitted = 0;
        record.serialized = 0;
        tcp_added_test_barrier();
    } else {
        records[count - 1] = record;
        if (records_save(p, records, count) != 0) {
            if (stop_record(p, guest_ip, &record) != 0)
                logerr("cannot roll back uncommitted UDP forward");
            else if (records_save(p, records, count - 1) != 0)
                logerr("cannot remove reserved port forward state");
            goto out;
        }
    }
    result = 0;

out:
    close(lock_fd);
    return result;
}

int port_forward_add_serialized(const struct profile *p, const char *guest_ip,
                                const struct port_spec *spec)
{
    return port_forward_add_under_operation_lock(p, guest_ip, spec);
}

int port_forward_add(const struct profile *p, const char *guest_ip,
                     const struct port_spec *spec)
{
    int operation_lock = port_forward_operation_lock(p);
    if (operation_lock < 0) {
        logerr("cannot lock port-forward listener creation");
        return -1;
    }
    int result = port_forward_add_under_operation_lock(p, guest_ip, spec);
    port_forward_operation_unlock(operation_lock);
    return result;
}

int port_forward_submit_many(const struct profile *p,
                             const struct port_spec specs[], int spec_count,
                             int serialized,
                             const struct port_forward_generation *generation)
{
    if (!specs || spec_count <= 0 || spec_count > MAX_FORWARD_RECORDS ||
        !generation_valid(generation))
        return -1;
    int lock_fd = state_lock(p);
    if (lock_fd < 0)
        return -1;
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    int result = -1;
    if (records_load(p, records, &count) != 0)
        goto out;
    for (int spec_index = 0; spec_index < spec_count; spec_index++) {
        int found = 0;
        for (int i = 0; i < count; i++) {
            if (!same_forward(&records[i].spec, &specs[spec_index]))
                continue;
            if (!generation_matches(&records[i], generation))
                goto out;
            records[i].submitted = 1;
            records[i].serialized = serialized != 0;
            found = 1;
            break;
        }
        if (!found)
            goto out;
    }
    result = records_save(p, records, count);

out:
    close(lock_fd);
    return result;
}

int port_forward_commit(const struct profile *p,
                        const struct port_spec *spec)
{
    int lock_fd = state_lock(p);
    if (lock_fd < 0)
        return -1;
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    int result = -1;
    if (records_load(p, records, &count) != 0)
        goto out;
    for (int i = 0; i < count; i++) {
        if (!same_forward(&records[i].spec, spec))
            continue;
        if (!records[i].pending) {
            result = 0;
            goto out;
        }
        records[i].pending = 0;
        records[i].submitted = 0;
        records[i].serialized = 0;
        records[i].owner_pid = 0;
        records[i].owner_start_sec = 0;
        records[i].owner_start_usec = 0;
        result = records_save(p, records, count);
        goto out;
    }

out:
    close(lock_fd);
    return result;
}

int port_forward_commit_owned(
    const struct profile *p, const struct port_spec *spec,
    const struct port_forward_generation *generation)
{
    if (!generation_valid(generation))
        return -1;
    int lock_fd = state_lock(p);
    if (lock_fd < 0)
        return -1;
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    int result = -1;
    if (records_load(p, records, &count) != 0)
        goto out;
    result = 0;
    for (int i = 0; i < count; i++) {
        if (!same_forward(&records[i].spec, spec) ||
            !generation_matches(&records[i], generation))
            continue;
        records[i].pending = 0;
        records[i].submitted = 0;
        records[i].serialized = 0;
        records[i].owner_pid = 0;
        records[i].owner_start_sec = 0;
        records[i].owner_start_usec = 0;
        result = records_save(p, records, count);
        break;
    }

out:
    close(lock_fd);
    return result;
}

static int remove_under_operation_lock(const struct profile *p,
                                       const char *guest_ip,
                                       const struct port_spec *spec,
                                       const struct port_forward_generation *generation)
{
    int lock_fd = state_lock(p);
    if (lock_fd < 0)
        return -1;
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    int result = -1;
    if (records_load(p, records, &count) != 0)
        goto out;
    result = 0;
    for (int i = 0; i < count; i++) {
        if (same_listener(&records[i].spec, spec) &&
            strcmp(records[i].spec.host_ip, spec->host_ip) == 0) {
            if (generation && !generation_matches(&records[i], generation))
                break;
            if (stop_record(p, guest_ip, &records[i]) != 0) {
                result = -1;
                break;
            }
            if (records[i].spec.protocol == PORT_TCP)
                tcp_cancelled_test_barrier();
            memmove(&records[i], &records[i + 1],
                    (size_t)(count - i - 1) * sizeof(records[0]));
            count--;
            result = records_save(p, records, count);
            break;
        }
    }

out:
    close(lock_fd);
    return result;
}

int port_forward_remove_serialized(const struct profile *p,
                                   const char *guest_ip,
                                   const struct port_spec *spec)
{
    return remove_under_operation_lock(p, guest_ip, spec, NULL);
}

int port_forward_remove_owned_serialized(
    const struct profile *p, const char *guest_ip,
    const struct port_spec *spec,
    const struct port_forward_generation *generation)
{
    if (!generation_valid(generation))
        return -1;
    return remove_under_operation_lock(p, guest_ip, spec, generation);
}

int port_forward_remove(const struct profile *p, const char *guest_ip,
                        const struct port_spec *spec)
{
    int operation_lock = port_forward_operation_lock(p);
    if (operation_lock < 0)
        return -1;
    int result = remove_under_operation_lock(p, guest_ip, spec, NULL);
    port_forward_operation_unlock(operation_lock);
    return result;
}

int port_forward_remove_owned(
    const struct profile *p, const char *guest_ip,
    const struct port_spec *spec,
    const struct port_forward_generation *generation)
{
    if (!generation_valid(generation))
        return -1;
    int operation_lock = port_forward_operation_lock(p);
    if (operation_lock < 0)
        return -1;
    int result = remove_under_operation_lock(p, guest_ip, spec, generation);
    port_forward_operation_unlock(operation_lock);
    return result;
}

int port_forward_cleanup(const struct profile *p, const char *guest_ip)
{
    int operation_lock = port_forward_operation_lock(p);
    if (operation_lock < 0)
        return -1;
    int lock_fd = state_lock(p);
    if (lock_fd < 0) {
        port_forward_operation_unlock(operation_lock);
        return -1;
    }
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    int result = -1;
    if (records_load(p, records, &count) != 0)
        goto out;
    int kept = 0;
    int stop_failed = 0;
    for (int i = 0; i < count; i++) {
        if (stop_record(p, guest_ip, &records[i]) != 0) {
            records[kept++] = records[i];
            stop_failed = 1;
        }
    }
    if (records_save(p, records, kept) == 0)
        result = stop_failed ? -1 : 0;

out:
    close(lock_fd);
    port_forward_operation_unlock(operation_lock);
    return result;
}

static int parse_published_range(const char *begin, const char *end,
                                 unsigned *first, unsigned *last)
{
    while (begin < end && (*begin == ' ' || *begin == '\t'))
        begin++;
    while (end > begin && (end[-1] == ' ' || end[-1] == '\t'))
        end--;
    if (begin == end || (size_t)(end - begin) >= 32)
        return -1;

    char text[32];
    memcpy(text, begin, (size_t)(end - begin));
    text[end - begin] = '\0';
    char *dash = strchr(text, '-');
    if (!dash) {
        if (port_number_parse(text, first) != 0)
            return -1;
        *last = *first;
        return 0;
    }
    *dash++ = '\0';
    if (strchr(dash, '-') || port_number_parse(text, first) != 0 ||
        port_number_parse(dash, last) != 0 || *last < *first)
        return -1;
    return 0;
}

static int published_group_contains(const char *begin, const char *end,
                                    const char *expected_ip,
                                    const struct forward_record *record)
{
    int have_bind_ip = 0;
    int bind_ip_matches = 0;
    const char *token = begin;
    while (token < end) {
        while (token < end &&
               (*token == ',' || *token == ' ' || *token == '\t'))
            token++;
        const char *token_end = token;
        while (token_end < end && *token_end != ',')
            token_end++;
        const char *trimmed_end = token_end;
        while (trimmed_end > token &&
               (trimmed_end[-1] == ' ' || trimmed_end[-1] == '\t'))
            trimmed_end--;

        const char *arrow = NULL;
        for (const char *p = token; p + 1 < trimmed_end; p++) {
            if (p[0] == '-' && p[1] == '>') {
                arrow = p;
                break;
            }
        }
        if (!arrow) {
            token = token_end < end ? token_end + 1 : end;
            continue;
        }

        const char *colon = NULL;
        for (const char *p = token; p < arrow; p++) {
            if (*p == ':')
                colon = p;
        }
        const char *host_range = token;
        if (colon) {
            size_t ip_len = (size_t)(colon - token);
            have_bind_ip = 1;
            bind_ip_matches = strlen(expected_ip) == ip_len &&
                              strncmp(token, expected_ip, ip_len) == 0;
            host_range = colon + 1;
        }

        unsigned host_first = 0, host_last = 0;
        unsigned container_first = 0, container_last = 0;
        if (have_bind_ip && bind_ip_matches &&
            parse_published_range(host_range, arrow, &host_first,
                                  &host_last) == 0 &&
            parse_published_range(arrow + 2, trimmed_end, &container_first,
                                  &container_last) == 0 &&
            host_last - host_first == container_last - container_first &&
            record->spec.host_port >= host_first &&
            record->spec.host_port <= host_last &&
            record->spec.container_port >= container_first &&
            record->spec.container_port <= container_last &&
            record->spec.host_port - host_first ==
                record->spec.container_port - container_first)
            return 1;

        token = token_end < end ? token_end + 1 : end;
    }
    return 0;
}

static const char *published_protocol_suffix(const char *begin,
                                             const char *end,
                                             enum port_protocol *protocol)
{
    for (const char *p = begin; p + 4 <= end; p++) {
        int bounded = p + 4 == end || p[4] == ',' || p[4] == ' ' ||
                      p[4] == '\t';
        if (bounded && memcmp(p, "/tcp", 4) == 0) {
            *protocol = PORT_TCP;
            return p;
        }
        if (bounded && memcmp(p, "/udp", 4) == 0) {
            *protocol = PORT_UDP;
            return p;
        }
    }
    return NULL;
}

static int published_contains(const char *published_ports,
                              const char *guest_ip,
                              const struct forward_record *record)
{
    const char *expected_ip = record->spec.protocol == PORT_UDP ?
                              guest_ip : "127.0.0.1";
    const char *line = published_ports;
    while (*line) {
        const char *line_end = line;
        while (*line_end && *line_end != '\r' && *line_end != '\n')
            line_end++;

        const char *group = line;
        while (group < line_end) {
            enum port_protocol protocol;
            const char *suffix = published_protocol_suffix(group, line_end,
                                                            &protocol);
            if (!suffix)
                break;
            if (protocol == record->spec.protocol &&
                published_group_contains(group, suffix, expected_ip,
                                         record))
                return 1;
            group = suffix + 4;
            while (group < line_end &&
                   (*group == ',' || *group == ' ' || *group == '\t'))
                group++;
        }
        line = line_end;
        while (*line == '\r' || *line == '\n')
            line++;
    }
    return 0;
}

static int reconcile(const struct profile *p, const char *guest_ip,
                     const char *published_ports,
                     int allow_serialized_cleanup,
                     const char *previous_published_ports)
{
    int lock_fd = state_lock(p);
    if (lock_fd < 0)
        return -1;
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    int result = -1;
    if (records_load(p, records, &count) != 0)
        goto out;
    int kept = 0;
    int stop_failed = 0;
    for (int i = 0; i < count; i++) {
        int published = published_contains(published_ports, guest_ip,
                                           &records[i]);
        enum process_identity owner = records[i].pending ?
                                      owner_identity(&records[i]) :
                                      PROCESS_IDENTITY_CHANGED;
        int resource_gone = 0;
        if (allow_serialized_cleanup && records[i].pending &&
            !records[i].submitted && !records[i].serialized &&
            owner == PROCESS_IDENTITY_CHANGED &&
            records[i].spec.protocol == PORT_TCP && !published &&
            tcp_listener_available(&records[i].spec))
            resource_gone = 1;
        if (records[i].pending && owner == PROCESS_IDENTITY_CHANGED &&
            records[i].spec.protocol == PORT_UDP && records[i].pid == 0) {
            enum udp_pending_recovery recovery =
                recover_pending_udp(p, &records[i]);
            if (recovery == UDP_PENDING_UNKNOWN)
                owner = PROCESS_IDENTITY_UNKNOWN;
            else if (recovery == UDP_PENDING_GONE)
                resource_gone = 1;
        }
        if (records[i].spec.protocol == PORT_UDP && published &&
            !resource_gone &&
            process_identity(&records[i]) != PROCESS_IDENTITY_MATCH) {
            /*
             * Guest inventory alone cannot prove host UDP readiness. Keep the
             * record for explicit stop recovery, but fail reconciliation so a
             * dead or unverified relay is never reported as committed-ready.
             */
            records[kept++] = records[i];
            stop_failed = 1;
            continue;
        }
        int submitted_absent = records[i].pending &&
            records[i].submitted && !published;
        int submitted_removed = submitted_absent &&
            previous_published_ports &&
            published_contains(previous_published_ports, guest_ip,
                               &records[i]);
        int submitted_absent_protected = submitted_absent &&
            !submitted_removed;
        int control_ambiguous_protected = records[i].pending &&
            records[i].serialized && !records[i].submitted;
        if (resource_gone && published) {
            records[kept++] = records[i];
            stop_failed = 1;
            continue;
        }
        if (resource_gone && submitted_absent_protected) {
            records[kept++] = records[i];
            continue;
        }
        if (resource_gone)
            continue;
        if (records[i].pending && published &&
            (owner == PROCESS_IDENTITY_CHANGED ||
             (allow_serialized_cleanup && records[i].submitted &&
              records[i].serialized))) {
            records[i].pending = 0;
            records[i].submitted = 0;
            records[i].serialized = 0;
            records[i].owner_pid = 0;
            records[i].owner_start_sec = 0;
            records[i].owner_start_usec = 0;
        }
        if ((records[i].pending &&
             !submitted_removed &&
             (owner != PROCESS_IDENTITY_CHANGED ||
              submitted_absent_protected || control_ambiguous_protected)) ||
            (!records[i].pending && published)) {
            records[kept++] = records[i];
        } else {
            if (stop_record(p, guest_ip, &records[i]) != 0) {
                records[kept++] = records[i];
                stop_failed = 1;
            }
        }
    }
    if (records_save(p, records, kept) == 0)
        result = stop_failed ? -1 : 0;

out:
    close(lock_fd);
    return result;
}

static int docker_specs_valid(const struct port_spec specs[], int spec_count)
{
    if (spec_count < 0 || spec_count > MAX_FORWARD_RECORDS ||
        (spec_count > 0 && !specs))
        return 0;
    for (int i = 0; i < spec_count; i++) {
        struct in_addr address;
        if ((specs[i].protocol != PORT_TCP && specs[i].protocol != PORT_UDP) ||
            !specs[i].host_ip[0] ||
            inet_pton(AF_INET, specs[i].host_ip, &address) != 1 ||
            specs[i].host_port == 0 || specs[i].host_port > 65535 ||
            specs[i].container_port == 0 || specs[i].container_port > 65535)
            return 0;
        for (int previous = 0; previous < i; previous++) {
            if (same_listener(&specs[previous], &specs[i]))
                return 0;
        }
    }
    return 1;
}

static void record_mark_committed(struct forward_record *record)
{
    record->pending = 0;
    record->submitted = 0;
    record->serialized = 0;
    record->owner_pid = 0;
    record->owner_start_sec = 0;
    record->owner_start_usec = 0;
}

int port_forward_sync_docker_serialized(const struct profile *p,
                                        const char *guest_ip,
                                        const struct port_spec specs[],
                                        int spec_count)
{
    if (!p || !guest_ip || !guest_ip[0] ||
        !docker_specs_valid(specs, spec_count)) {
        errno = EINVAL;
        return -1;
    }

    int lock_fd = state_lock(p);
    if (lock_fd < 0)
        return -1;
    struct forward_record records[MAX_FORWARD_RECORDS];
    int present[MAX_FORWARD_RECORDS] = {0};
    int count = 0;
    int kept = 0;
    int failed = 0;
    if (records_load(p, records, &count) != 0) {
        close(lock_fd);
        return -1;
    }

    for (int i = 0; i < count; i++) {
        int desired = -1;
        for (int candidate = 0; candidate < spec_count; candidate++) {
            if (same_forward(&records[i].spec, &specs[candidate])) {
                desired = candidate;
                break;
            }
        }
        if (desired >= 0) {
            present[desired] = 1;
            if (records[i].pending &&
                (records[i].spec.protocol != PORT_UDP ||
                 process_identity(&records[i]) == PROCESS_IDENTITY_MATCH))
                record_mark_committed(&records[i]);
            else if (records[i].pending) {
                /* Do not claim an unverified UDP relay is ready. */
                failed = 1;
            }
            records[kept++] = records[i];
            continue;
        }
        if (stop_record(p, guest_ip, &records[i]) != 0) {
            records[kept++] = records[i];
            failed = 1;
        }
    }
    if (records_save(p, records, kept) != 0) {
        close(lock_fd);
        return -1;
    }
    close(lock_fd);

    for (int i = 0; i < spec_count; i++) {
        if (present[i])
            continue;
        if (port_forward_add_under_operation_lock(p, guest_ip, &specs[i]) !=
            0) {
            failed = 1;
            continue;
        }
        if (port_forward_commit(p, &specs[i]) != 0)
            failed = 1;
    }
    return failed ? -1 : 0;
}

int port_forward_reconcile(const struct profile *p, const char *guest_ip,
                           const char *published_ports)
{
    int operation_lock = port_forward_operation_lock(p);
    if (operation_lock < 0)
        return -1;
    int result = reconcile(p, guest_ip, published_ports, 0, NULL);
    port_forward_operation_unlock(operation_lock);
    return result;
}

int port_forward_reconcile_serialized(const struct profile *p,
                                      const char *guest_ip,
                                      const char *published_ports)
{
    return reconcile(p, guest_ip, published_ports, 1, NULL);
}

int port_forward_reconcile_rm_serialized(const struct profile *p,
                                         const char *guest_ip,
                                         const char *previous_published_ports,
                                         const char *published_ports)
{
    if (!previous_published_ports)
        return -1;
    return reconcile(p, guest_ip, published_ports, 1,
                     previous_published_ports);
}
