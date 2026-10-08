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
#include <sys/sysctl.h>
#include <sys/wait.h>
#include <unistd.h>

#include "cjson/cJSON.h"
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
        /* Nothing sets `submitted` any more. The two shapes stay readable, as
         * the pending records they are, so that the accepted file format is
         * unchanged. */
        record->pending = 1;
        record->submitted = 1;
        record->serialized = strcmp(ownership, "submitted-locked") == 0;
    } else if (strcmp(ownership, "committed") != 0) {
        return -1;
    }
    return 0;
}

/*
 * port-forwards.tsv holds one record per line, written only by
 * records_save():
 *   protocol host_ip host_port container_port pid start_sec start_usec
 *   ownership owner_pid owner_start_sec owner_start_usec
 * separated by tabs. The relay pid/start token identifies a UDP relay (TCP
 * records carry pid 0); the owner fields identify the process that reserved a
 * pending record. Any other record shape, including the pre-release 5-, 7-
 * and 8-field ones, is corrupt: the whole load fails, so no caller rewrites
 * the file or signals a process on partial evidence.
 */
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
        if (parsed != 11 || ownership_parse(ownership, &record) != 0) {
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

enum process_identity {
    PROCESS_IDENTITY_UNKNOWN = -1,
    PROCESS_IDENTITY_CHANGED = 0,
    PROCESS_IDENTITY_MATCH = 1,
};

/* A process that has ended and that its parent has not reaped. libproc
 * reports it as absent while it still answers signals; it runs nothing and
 * holds no socket. */
static int process_is_zombie(int pid)
{
    int name[] = { CTL_KERN, KERN_PROC, KERN_PROC_PID, pid };
    struct kinfo_proc info;
    size_t size = sizeof(info);
    return sysctl(name, 4, &info, &size, NULL, 0) == 0 &&
           size == sizeof(info) && info.kp_proc.p_pid == pid &&
           info.kp_proc.p_stat == SZOMB;
}

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
    /* Whichever process the zombie was, the recorded one does not run. */
    if (process_is_zombie(pid))
        return PROCESS_IDENTITY_CHANGED;
    return PROCESS_IDENTITY_UNKNOWN;
}

static enum process_identity process_identity(const struct forward_record *record)
{
    return process_identity_values(record->pid, record->start_sec,
                                   record->start_usec);
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

/*
 * Whether a host listener can carry `spec` to the guest. A UDP relay listens
 * on the published address on the host and sends to the guest's NAT address.
 * Docker in the guest receives a port that is published on one address on
 * that address of the guest alone. For 127.0.0.1, the guest's own loopback,
 * the relay's datagrams find no listener; the guest's NAT address is one that
 * the host cannot bind. Only a port published on all addresses is received
 * where the relay sends. A TCP forward is not judged here: it enters the
 * guest through SSH and connects to the guest's loopback.
 */
static int spec_forwardable(const struct port_spec *spec)
{
    struct in_addr address;
    return spec->protocol != PORT_UDP ||
           (inet_pton(AF_INET, spec->host_ip, &address) == 1 &&
            address.s_addr == htonl(INADDR_ANY));
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

/* Reaps `pid` when it is a child of this process that has ended. It does
 * nothing to a child that runs or to a process that is not a child. */
static void reap_ended_child(int pid)
{
    int status = 0;
    while (waitpid(pid, &status, WNOHANG) < 0 && errno == EINTR)
        ;
}

enum udp_relay {
    UDP_RELAY_UNVERIFIED = -1, /* never signalled, replaced or reported ready */
    UDP_RELAY_GONE = 0,
    UDP_RELAY_RUNNING = 1,
};

/*
 * Establishes whether the relay of a UDP record runs. The caller holds the
 * operation lock, so no relay of the profile is being started meanwhile.
 *
 * The observer starts the relays and lives on, so a relay that ends stays its
 * child until the observer reaps it, which it does here.
 *
 * A record that names its relay is judged by that process identity. A record
 * without one was reserved by a process that ended before it recorded the
 * relay it may have started. The relay's own pidfile then decides: a pidfile
 * that identifies a live process is that relay, and its identity is copied
 * into `record`; one that identifies a process that is gone leaves no relay;
 * and without a pidfile, a host port that can be bound has no relay.
 * Everything else cannot be verified.
 */
static enum udp_relay udp_relay_state(const struct profile *p,
                                      struct forward_record *record)
{
    enum process_identity identity;
    if (record->pid != 0) {
        reap_ended_child(record->pid);
        identity = process_identity(record);
    } else {
        char pidfile[1100];
        int pid = 0;
        uint64_t start_sec = 0, start_usec = 0;
        if (udp_pidfile(p, &record->spec, pidfile, sizeof(pidfile)) != 0)
            return UDP_RELAY_UNVERIFIED;
        if (udp_pidfile_identity(pidfile, &pid, &start_sec, &start_usec) != 0)
            return errno == ENOENT && udp_listener_available(&record->spec) ?
                   UDP_RELAY_GONE : UDP_RELAY_UNVERIFIED;
        reap_ended_child(pid);
        identity = process_identity_values(pid, start_sec, start_usec);
        if (identity == PROCESS_IDENTITY_MATCH) {
            record->pid = pid;
            record->start_sec = start_sec;
            record->start_usec = start_usec;
        }
    }
    return identity == PROCESS_IDENTITY_MATCH ? UDP_RELAY_RUNNING :
           identity == PROCESS_IDENTITY_CHANGED ? UDP_RELAY_GONE :
           UDP_RELAY_UNVERIFIED;
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
            const char *detail = ssh_forward_cancel_detail();
            logerr("cannot stop TCP forward on %s:%u%s%s",
                   record->spec.host_ip, record->spec.host_port,
                   detail[0] ? ": " : "", detail);
            return -1;
        }
        return 0;
    }

    struct forward_record relay = *record;
    enum udp_relay state = udp_relay_state(p, &relay);
    if (state == UDP_RELAY_UNVERIFIED) {
        if (relay.pid != 0)
            logerr("refusing to stop unverified UDP forward process %d",
                   relay.pid);
        else
            logerr("refusing to stop the UDP forward on %s:%u: its relay "
                   "cannot be identified", relay.spec.host_ip,
                   relay.spec.host_port);
        return -1;
    }
    if (state == UDP_RELAY_RUNNING && terminate_udp_process(&relay) != 0) {
        logerr("cannot stop UDP forward process %d", relay.pid);
        return -1;
    }
    if (remove_udp_pidfile(p, &relay) != 0) {
        logerr("cannot remove UDP forward pidfile for %d", relay.pid);
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

enum forward_failure {
    FORWARD_FAILURE_HOST_PORT_IN_USE,
    FORWARD_FAILURE_OTHER,
    FORWARD_FAILURE_RELAY_UNVERIFIED, /* reported as FORWARD_FAILURE_OTHER */
    FORWARD_FAILURE_UDP_ADDRESS_UNSUPPORTED, /* !spec_forwardable() */
};

#define FORWARD_DETAIL_CAP 256

static void records_remove(struct forward_record records[], int *count,
                           int index)
{
    memmove(&records[index], &records[index + 1],
            (size_t)(*count - index - 1) * sizeof(records[0]));
    (*count)--;
}

/* Creates the host listener for `spec` and leaves its record pending for the
 * caller to commit. A record that already claims the listener refuses the
 * request, with one exception.
 *
 * With `adopt_unconfirmed`, a pending TCP record of this exact forward is
 * taken over instead: it says that a control request was reserved or sent,
 * not that the listener exists. The request is sent again under this
 * process's ownership. The SSH master answers a forward it already holds with
 * success and creates nothing, so success is the master's own statement that
 * it holds the listener, whichever request created it. A failure is handled
 * like that of a first request, and the record never leaves the state file
 * while the outcome is open.
 *
 * On failure, `*failure` is why the host listener could not be created and
 * `detail_out` (FORWARD_DETAIL_CAP bytes) is what the SSH client said, empty
 * when it said nothing. The caller reports them. */
static int add_forward_under_operation_lock(const struct profile *p,
                                            const char *guest_ip,
                                            const struct port_spec *spec,
                                            int adopt_unconfirmed,
                                            enum forward_failure *failure,
                                            char *detail_out)
{
    detail_out[0] = '\0';
    *failure = FORWARD_FAILURE_OTHER;
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
    int slot = -1;
    for (int i = 0; i < count; i++) {
        if (!same_listener(&records[i].spec, spec))
            continue;
        if (adopt_unconfirmed && slot < 0 && spec->protocol == PORT_TCP &&
            records[i].pending && same_forward(&records[i].spec, spec)) {
            slot = i;
            continue;
        }
        logerr("host %s port %s:%u is already published",
               protocol_name(spec->protocol), spec->host_ip,
               spec->host_port);
        goto out;
    }
    int adopted = slot >= 0;
    if (!adopted) {
        if (count >= MAX_FORWARD_RECORDS)
            goto out;
        slot = count++;
    }

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
    if (!adopted) {
        /*
         * Persist ownership before creating the listener. If the filesystem
         * cannot reserve recovery state, no host resource is created. An
         * adopted record is already reserved and keeps its phase until the
         * control-request phase below replaces it in one write.
         */
        records[slot] = record;
        if (records_save(p, records, count) != 0)
            goto out;

        if (spec->protocol == PORT_UDP)
            udp_reservation_test_barrier();
        else
            tcp_reservation_test_barrier();
    }

    int rc;
    int cancelled = 0;
    if (spec->protocol == PORT_TCP) {
        /*
         * A killed process or a failed SSH response may leave the master
         * request applied after this process is gone. Persist an uncertain
         * external mutation phase before sending the control request. Only
         * the master's answer to that request or to its cancel, or evidence
         * that no listener exists, may resolve it.
         */
        record.submitted = 0;
        record.serialized = 1;
        records[slot] = record;
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
        if (spec->protocol == PORT_TCP) {
            snprintf(detail_out, FORWARD_DETAIL_CAP, "%s",
                     ssh_forward_add_detail());
            cancelled = ssh_forward_cancel_tcp(
                p, guest_ip, spec->host_ip, spec->host_port, "127.0.0.1",
                spec->host_port) == 0;
            int absent = cancelled ||
                (ssh_master_alive(p) != 0 && tcp_listener_available(spec));
            if (absent) {
                records_remove(records, &count, slot);
                if (records_save(p, records, count) != 0)
                    logerr("cannot remove failed TCP forward state");
            } else {
                records[slot] = record;
                if (records_save(p, records, count) != 0)
                    logerr("cannot persist uncertain TCP forward state");
            }
        } else if (rc == UDP_START_UNSAFE) {
            records[slot] = record;
            if (records_save(p, records, count) != 0)
                logerr("cannot persist uncertain UDP forward identity");
        } else {
            records_remove(records, &count, slot);
            if (records_save(p, records, count) != 0)
                logerr("cannot remove reserved port forward state");
        }
        /* A host port that still cannot be bound belongs to another process
         * only once the master answered the cancel: then it holds no such
         * forward. While the cancel is unanswered, the master itself may hold
         * the port, and the holder is not named. */
        if (spec->protocol == PORT_TCP ?
            cancelled && !tcp_listener_available(spec) :
            !udp_listener_available(spec))
            *failure = FORWARD_FAILURE_HOST_PORT_IN_USE;
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
        records[slot] = record;
        if (records_save(p, records, count) != 0) {
            if (stop_record(p, guest_ip, &record) != 0) {
                logerr("cannot roll back uncommitted UDP forward");
            } else {
                records_remove(records, &count, slot);
                if (records_save(p, records, count) != 0)
                    logerr("cannot remove reserved port forward state");
            }
            goto out;
        }
    }
    result = 0;

out:
    close(lock_fd);
    return result;
}

/* Commits the pending record of `spec` once its listener exists. A record
 * that is already committed stays as it is; a missing record is an error. */
static int commit_forward(const struct profile *p,
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

int port_forward_unconfirm_tcp_serialized(const struct profile *p)
{
    if (!p) {
        errno = EINVAL;
        return -1;
    }
    int lock_fd = state_lock(p);
    if (lock_fd < 0)
        return -1;
    struct forward_record records[MAX_FORWARD_RECORDS];
    int count = 0;
    int result = -1;
    int owner_pid = (int)getpid();
    uint64_t owner_start_sec = 0, owner_start_usec = 0;
    if (records_load(p, records, &count) != 0 ||
        process_start_token(owner_pid, &owner_start_sec,
                            &owner_start_usec) != 0)
        goto out;
    int changed = 0;
    for (int i = 0; i < count; i++) {
        if (records[i].spec.protocol != PORT_TCP || records[i].pending)
            continue;
        /* The phase of a control request whose outcome is not known. */
        records[i].pending = 1;
        records[i].serialized = 1;
        records[i].owner_pid = owner_pid;
        records[i].owner_start_sec = owner_start_sec;
        records[i].owner_start_usec = owner_start_usec;
        changed = 1;
    }
    result = changed ? records_save(p, records, count) : 0;

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

#define FAILURES_FILE "port-forward-failures.json"
#define FAILURES_FILE_CAP (32 * 1024)

struct failed_forward {
    struct port_spec spec;
    enum forward_failure reason;
    char detail[FORWARD_DETAIL_CAP]; /* logged, never recorded */
};

static const char *failure_reason(enum forward_failure reason)
{
    return reason == FORWARD_FAILURE_HOST_PORT_IN_USE ? "hostPortInUse" :
        reason == FORWARD_FAILURE_UDP_ADDRESS_UNSUPPORTED ?
        "udpAddressUnsupported" : "forwardFailed";
}

/* What the log says about a published port that is not forwarded. */
static const char *failure_log_text(enum forward_failure reason)
{
    switch (reason) {
    case FORWARD_FAILURE_HOST_PORT_IN_USE:
        return "another process holds the host port";
    case FORWARD_FAILURE_RELAY_UNVERIFIED:
        return "its relay cannot be verified";
    case FORWARD_FAILURE_UDP_ADDRESS_UNSUPPORTED:
        return "a UDP port is forwarded only when it is published on all "
               "addresses";
    case FORWARD_FAILURE_OTHER:
        break;
    }
    return "the forward request failed";
}

static int failed_forward_order(const void *left_item, const void *right_item)
{
    const struct failed_forward *left = left_item, *right = right_item;
    if (left->spec.protocol != right->spec.protocol)
        return left->spec.protocol < right->spec.protocol ? -1 : 1;
    if (left->spec.host_port != right->spec.host_port)
        return left->spec.host_port < right->spec.host_port ? -1 : 1;
    return strcmp(left->spec.host_ip, right->spec.host_ip);
}

/* The record's text, or "[]\n" when the file is absent. NULL when it cannot
 * be read or exceeds the bound. The caller frees it. */
static char *failures_read(const struct profile *p)
{
    char path[1100];
    if (!profile_path(p, FAILURES_FILE, path, sizeof(path)))
        return NULL;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return errno == ENOENT ? strdup("[]\n") : NULL;
    char *text = malloc(FAILURES_FILE_CAP + 1);
    size_t length = 0;
    while (text && length <= FAILURES_FILE_CAP) {
        ssize_t count = read(fd, text + length, FAILURES_FILE_CAP + 1 - length);
        if (count < 0 && errno == EINTR)
            continue;
        if (count <= 0) {
            if (count < 0)
                length = FAILURES_FILE_CAP + 1;
            break;
        }
        length += (size_t)count;
    }
    close(fd);
    if (!text || length > FAILURES_FILE_CAP) {
        free(text);
        return NULL;
    }
    text[length] = '\0';
    return text;
}

static char *failures_text(const struct failed_forward failures[], int count)
{
    cJSON *array = cJSON_CreateArray();
    for (int i = 0; array && i < count; i++) {
        cJSON *entry = cJSON_CreateObject();
        if (!entry ||
            !cJSON_AddStringToObject(entry, "hostIp", failures[i].spec.host_ip) ||
            !cJSON_AddNumberToObject(entry, "hostPort",
                                     failures[i].spec.host_port) ||
            !cJSON_AddStringToObject(entry, "protocol",
                                     protocol_name(failures[i].spec.protocol)) ||
            !cJSON_AddStringToObject(entry, "reason",
                                     failure_reason(failures[i].reason)) ||
            !cJSON_AddItemToArray(array, entry)) {
            cJSON_Delete(entry);
            cJSON_Delete(array);
            array = NULL;
        }
    }
    char *compact = array ? cJSON_PrintUnformatted(array) : NULL;
    cJSON_Delete(array);
    if (!compact)
        return NULL;
    size_t length = strlen(compact);
    char *text = malloc(length + 2);
    if (text) {
        memcpy(text, compact, length);
        text[length] = '\n';
        text[length + 1] = '\0';
    }
    cJSON_free(compact);
    return text;
}

/* Records the forwards this synchronization could not create. A repeated
 * pass with the same failures writes and logs nothing: the observer retries
 * twice a second, and each change is what a reader of the log needs. */
static void failures_publish(const struct profile *p,
                             struct failed_forward failures[], int count)
{
    char path[1100];
    qsort(failures, (size_t)count, sizeof(failures[0]), failed_forward_order);
    char *text = failures_text(failures, count);
    char *previous = failures_read(p);
    if (text && previous && strcmp(text, previous) == 0)
        goto out;
    if (!text || !profile_path(p, FAILURES_FILE, path, sizeof(path)) ||
        fs_write_file_atomic(path, text, strlen(text), 0600) != 0) {
        logerr("cannot record the published ports that are not forwarded: %s",
               strerror(errno));
        goto out;
    }
    for (int i = 0; i < count; i++) {
        enum forward_failure reason = failures[i].reason;
        int explained = reason == FORWARD_FAILURE_OTHER &&
            failures[i].detail[0];
        logerr("cannot forward published %s port %s:%u: %s%s%s",
               protocol_name(failures[i].spec.protocol),
               failures[i].spec.host_ip, failures[i].spec.host_port,
               failure_log_text(reason),
               explained ? ": " : "", explained ? failures[i].detail : "");
    }
    if (count == 0)
        logmsg("every published port is forwarded again");
out:
    free(text);
    free(previous);
}

int port_forward_failures_clear(const struct profile *p)
{
    char path[1100];
    if (!p || !profile_path(p, FAILURES_FILE, path, sizeof(path))) {
        errno = EINVAL;
        return -1;
    }
    return fs_unlink_if_exists(path);
}

cJSON *port_forward_failures(const struct profile *p)
{
    cJSON *failures = cJSON_CreateArray();
    char *text = p ? failures_read(p) : NULL;
    cJSON *recorded = text ? cJSON_Parse(text) : NULL;
    free(text);
    if (!failures || !cJSON_IsArray(recorded))
        goto out;
    cJSON *valid = cJSON_CreateArray();
    for (const cJSON *entry = recorded->child; valid && entry;
         entry = entry->next) {
        const cJSON *host_ip = cJSON_GetObjectItemCaseSensitive(entry, "hostIp");
        const cJSON *host_port =
            cJSON_GetObjectItemCaseSensitive(entry, "hostPort");
        const cJSON *protocol =
            cJSON_GetObjectItemCaseSensitive(entry, "protocol");
        const cJSON *reason = cJSON_GetObjectItemCaseSensitive(entry, "reason");
        struct in_addr address;
        cJSON *copy = NULL;
        /* A record that is not exactly what the synchronization writes is not
         * reported in part. */
        if (!cJSON_IsObject(entry) || !cJSON_IsString(host_ip) ||
            inet_pton(AF_INET, host_ip->valuestring, &address) != 1 ||
            !cJSON_IsNumber(host_port) || host_port->valuedouble < 1 ||
            host_port->valuedouble > 65535 ||
            host_port->valuedouble != (double)(int)host_port->valuedouble ||
            !cJSON_IsString(protocol) ||
            (strcmp(protocol->valuestring, "tcp") != 0 &&
             strcmp(protocol->valuestring, "udp") != 0) ||
            !cJSON_IsString(reason) ||
            (strcmp(reason->valuestring, "hostPortInUse") != 0 &&
             strcmp(reason->valuestring, "forwardFailed") != 0 &&
             strcmp(reason->valuestring, "udpAddressUnsupported") != 0) ||
            !(copy = cJSON_CreateObject()) ||
            !cJSON_AddStringToObject(copy, "hostIp", host_ip->valuestring) ||
            !cJSON_AddNumberToObject(copy, "hostPort", host_port->valuedouble) ||
            !cJSON_AddStringToObject(copy, "protocol", protocol->valuestring) ||
            !cJSON_AddStringToObject(copy, "reason", reason->valuestring) ||
            !cJSON_AddItemToArray(valid, copy)) {
            cJSON_Delete(copy);
            cJSON_Delete(valid);
            valid = NULL;
        }
    }
    if (valid) {
        cJSON_Delete(failures);
        failures = valid;
    }
out:
    cJSON_Delete(recorded);
    return failures;
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
    /* What the state says about each published port. */
    enum { SPEC_UNRECORDED, SPEC_FORWARDED, SPEC_UNCONFIRMED };
    int recorded[MAX_FORWARD_RECORDS] = {0};
    int count = 0;
    int kept = 0;
    int failed = 0;
    /* At most one entry for each published port. */
    struct failed_forward failures[MAX_FORWARD_RECORDS];
    int failure_count = 0;
    if (records_load(p, records, &count) != 0) {
        close(lock_fd);
        return -1;
    }

    for (int i = 0; i < count; i++) {
        int desired = -1;
        for (int candidate = 0; candidate < spec_count; candidate++) {
            /* A record of a port that no listener can carry, left by a
             * version that started a relay for it, is stopped like a record
             * of a port that is no longer published. */
            if (same_forward(&records[i].spec, &specs[candidate]) &&
                spec_forwardable(&specs[candidate])) {
                desired = candidate;
                break;
            }
        }
        if (desired >= 0 && records[i].spec.protocol == PORT_UDP) {
            /*
             * A UDP record says that a relay was started, not that it still
             * runs. Once the relay is gone and its pidfile cleared, the
             * record goes too, and the port is forwarded below like one that
             * was never recorded.
             */
            enum udp_relay relay = udp_relay_state(p, &records[i]);
            if (relay == UDP_RELAY_GONE &&
                stop_record(p, guest_ip, &records[i]) == 0)
                continue;
            recorded[desired] = SPEC_FORWARDED;
            if (relay == UDP_RELAY_RUNNING) {
                if (records[i].pending)
                    record_mark_committed(&records[i]);
            } else {
                /* Do not claim an unverified UDP relay is ready. */
                failures[failure_count].spec = specs[desired];
                failures[failure_count].detail[0] = '\0';
                failures[failure_count++].reason = relay == UDP_RELAY_GONE ?
                    FORWARD_FAILURE_OTHER : FORWARD_FAILURE_RELAY_UNVERIFIED;
                failed = 1;
            }
            records[kept++] = records[i];
            continue;
        }
        if (desired >= 0 && records[i].pending &&
            records[i].spec.protocol == PORT_TCP) {
            /*
             * A pending TCP record says that a control request was reserved
             * or sent, not that the master holds the listener: the request
             * may have failed, or its answer was lost. Committing it here
             * would end the retries for a port that nothing forwards. The
             * record stays as it is until the request is sent again below.
             */
            recorded[desired] = SPEC_UNCONFIRMED;
            records[kept++] = records[i];
            continue;
        }
        if (desired >= 0) {
            /* A committed TCP record: port_forward_unconfirm_tcp_serialized()
             * is what withdraws the trust placed in it. */
            recorded[desired] = SPEC_FORWARDED;
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
        if (!spec_forwardable(&specs[i])) {
            /* Reported, and not a failure of this call: no later call can
             * forward the port while it is published as it is. */
            failures[failure_count].spec = specs[i];
            failures[failure_count].detail[0] = '\0';
            failures[failure_count++].reason =
                FORWARD_FAILURE_UDP_ADDRESS_UNSUPPORTED;
            continue;
        }
        if (recorded[i] == SPEC_FORWARDED)
            continue;
        enum forward_failure reason;
        if (add_forward_under_operation_lock(
                p, guest_ip, &specs[i], recorded[i] == SPEC_UNCONFIRMED,
                &reason, failures[failure_count].detail) != 0) {
            failures[failure_count].spec = specs[i];
            failures[failure_count++].reason = reason;
            failed = 1;
            continue;
        }
        if (commit_forward(p, &specs[i]) != 0)
            failed = 1;
    }
    failures_publish(p, failures, failure_count);
    return failed ? -1 : 0;
}
