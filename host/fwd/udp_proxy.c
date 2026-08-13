#include "fwd/udp_proxy.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <libproc.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc_info.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#include "core/log.h"
#include "fwd/ports.h"
#include "util/fs.h"

#define MAX_FLOWS 64
#define FLOW_IDLE_SECONDS 60

struct udp_flow {
    int fd;
    struct sockaddr_in client;
    time_t active_at;
};

static volatile sig_atomic_t stopping;

#ifdef HAMN_TEST
static int test_send_failure_loaded;
static unsigned test_send_failures;
static int test_recv_failure_loaded;
static unsigned test_recv_failures;

static int test_failure_once(const char *name, int *loaded,
                             unsigned *remaining, unsigned count)
{
    if (!*loaded) {
        *remaining = getenv(name) ? count : 0;
        *loaded = 1;
    }
    if (*remaining == 0)
        return 0;
    (*remaining)--;
    errno = EIO;
    return 1;
}
#endif

static void on_signal(int signal_number)
{
    (void)signal_number;
    stopping = 1;
}

static int parse_fd(const char *text, int *fd)
{
    char *end = NULL;
    errno = 0;
    long value = strtol(text, &end, 10);
    if (errno || !end || *end || value < 0 || value > INT32_MAX)
        return -1;
    *fd = (int)value;
    return 0;
}

static int same_address(const struct sockaddr_in *a,
                        const struct sockaddr_in *b)
{
    return a->sin_addr.s_addr == b->sin_addr.s_addr &&
           a->sin_port == b->sin_port;
}

static void close_flow(struct udp_flow *flow)
{
    if (flow->fd >= 0)
        close(flow->fd);
    flow->fd = -1;
    memset(&flow->client, 0, sizeof(flow->client));
    flow->active_at = 0;
}

static struct udp_flow *find_flow(struct udp_flow flows[],
                                  const struct sockaddr_in *client)
{
    struct udp_flow *free_flow = NULL;
    struct udp_flow *oldest = &flows[0];
    for (int i = 0; i < MAX_FLOWS; i++) {
        if (flows[i].fd >= 0 && same_address(&flows[i].client, client))
            return &flows[i];
        if (flows[i].fd < 0 && !free_flow)
            free_flow = &flows[i];
        if (flows[i].active_at < oldest->active_at)
            oldest = &flows[i];
    }
    if (free_flow)
        return free_flow;
    close_flow(oldest);
    return oldest;
}

static int open_flow(struct udp_flow *flow, const struct sockaddr_in *client,
                     const struct sockaddr_in *target)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0 || connect(fd, (const struct sockaddr *)target,
                          sizeof(*target)) != 0) {
        if (fd >= 0)
            close(fd);
        return -1;
    }
    flow->fd = fd;
    flow->client = *client;
    flow->active_at = time(NULL);
    return 0;
}

static ssize_t send_target(int fd, const unsigned char *buffer, size_t length)
{
#ifdef HAMN_TEST
    if (test_failure_once("HAMN_TEST_UDP_SEND_FAILURE",
                          &test_send_failure_loaded, &test_send_failures, 2))
        return -1;
#endif
    return send(fd, buffer, length, 0);
}

static ssize_t recv_target(int fd, unsigned char *buffer, size_t length)
{
#ifdef HAMN_TEST
    if (test_failure_once("HAMN_TEST_UDP_RECV_FAILURE",
                          &test_recv_failure_loaded, &test_recv_failures, 1))
        return -1;
#endif
    return recv(fd, buffer, length, 0);
}

static int send_to_target(struct udp_flow *flow,
                          const struct sockaddr_in *client,
                          const struct sockaddr_in *target,
                          const unsigned char *buffer, size_t length)
{
    int saved_errno = EIO;
    for (int attempt = 0; attempt < 2; attempt++) {
        if (flow->fd < 0 && open_flow(flow, client, target) != 0) {
            saved_errno = errno;
            continue;
        }
        ssize_t sent = send_target(flow->fd, buffer, length);
        if (sent == (ssize_t)length) {
            flow->active_at = time(NULL);
            return 0;
        }
        saved_errno = sent < 0 ? errno : EIO;
        close_flow(flow);
    }
    logerr("UDP relay cannot send to target: %s", strerror(saved_errno));
    return -1;
}

static int poll_flows(struct pollfd *pfds, nfds_t count)
{
#ifdef HAMN_TEST
    if (getenv("HAMN_TEST_UDP_POLL_FAILURE")) {
        errno = EIO;
        return -1;
    }
#endif
    return poll(pfds, count, 1000);
}

static int write_pidfile(const char *path)
{
    struct proc_bsdinfo info;
    int size = proc_pidinfo(getpid(), PROC_PIDTBSDINFO, 0, &info,
                            sizeof(info));
    if (size != (int)sizeof(info) || info.pbi_pid != (uint32_t)getpid())
        return -1;
    char text[96];
    int n = snprintf(text, sizeof(text), "%ld\t%" PRIu64 "\t%" PRIu64
                     "\n", (long)getpid(),
                     (uint64_t)info.pbi_start_tvsec,
                     (uint64_t)info.pbi_start_tvusec);
    if (n < 0 || n >= (int)sizeof(text)) {
        return -1;
    }
    return fs_write_file_atomic(path, text, (size_t)n, 0600);
}

static int write_fifo_message(const char *path, const char *message)
{
    int fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    size_t remaining = strlen(message);
    const char *cursor = message;
    while (remaining > 0) {
        ssize_t written = write(fd, cursor, remaining);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0) {
            close(fd);
            return -1;
        }
        cursor += written;
        remaining -= (size_t)written;
    }
    return close(fd);
}

static int udp_start_test_barrier(void)
{
    const char *ready = getenv("HAMN_TEST_UDP_RELAY_READY_FIFO");
    const char *release = getenv("HAMN_TEST_UDP_RELAY_RELEASE_FIFO");
    if (!ready && !release)
        return 0;
    if (!ready || !release)
        return -1;
    if (getenv("HAMN_TEST_UDP_RELAY_IGNORE_TERM") &&
        signal(SIGTERM, SIG_IGN) == SIG_ERR)
        return -1;
    char message[32];
    int n = snprintf(message, sizeof(message), "%ld\n", (long)getpid());
    if (n <= 0 || n >= (int)sizeof(message) ||
        write_fifo_message(ready, message) != 0)
        return -1;
    int fd = open(release, O_RDONLY | O_CLOEXEC);
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

static int udp_pidfile_test_ready(void)
{
    const char *ready = getenv("HAMN_TEST_UDP_RELAY_PIDFILE_READY_FIFO");
    return !ready || write_fifo_message(ready, "ready\n") == 0 ? 0 : -1;
}

int cmd_udp_forward(int argc, char **argv)
{
    const char *listen_ip = NULL, *target_ip = NULL, *pidfile = NULL;
    unsigned listen_port = 0, target_port = 0;
    int listener = -1;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--listen-address") == 0 && i + 1 < argc)
            listen_ip = argv[++i];
        else if (strcmp(argv[i], "--listen-port") == 0 && i + 1 < argc) {
            if (port_number_parse(argv[++i], &listen_port) != 0)
                return 2;
        } else if (strcmp(argv[i], "--listen-fd") == 0 && i + 1 < argc) {
            if (parse_fd(argv[++i], &listener) != 0)
                return 2;
        } else if (strcmp(argv[i], "--target-address") == 0 &&
                   i + 1 < argc)
            target_ip = argv[++i];
        else if (strcmp(argv[i], "--target-port") == 0 && i + 1 < argc) {
            if (port_number_parse(argv[++i], &target_port) != 0)
                return 2;
        } else if (strcmp(argv[i], "--pidfile") == 0 && i + 1 < argc)
            pidfile = argv[++i];
        else
            return 2;
    }
    if (!listen_ip || !target_ip || !pidfile || !listen_port || !target_port ||
        listener < 0)
        return 2;

    struct sockaddr_in listen_addr = { .sin_family = AF_INET },
                       target_addr = { .sin_family = AF_INET };
    listen_addr.sin_port = htons((uint16_t)listen_port);
    target_addr.sin_port = htons((uint16_t)target_port);
    if (inet_pton(AF_INET, listen_ip, &listen_addr.sin_addr) != 1 ||
        inet_pton(AF_INET, target_ip, &target_addr.sin_addr) != 1)
        return 2;

    int socket_type = 0;
    socklen_t socket_type_len = sizeof(socket_type);
    struct sockaddr_in inherited_address;
    socklen_t inherited_address_len = sizeof(inherited_address);
    if (getsockopt(listener, SOL_SOCKET, SO_TYPE, &socket_type,
                   &socket_type_len) != 0 || socket_type != SOCK_DGRAM ||
        getsockname(listener, (struct sockaddr *)&inherited_address,
                    &inherited_address_len) != 0 ||
        inherited_address_len != sizeof(inherited_address) ||
        inherited_address.sin_family != AF_INET ||
        inherited_address.sin_port != listen_addr.sin_port ||
        inherited_address.sin_addr.s_addr != listen_addr.sin_addr.s_addr) {
        close(listener);
        return 1;
    }
    if (udp_start_test_barrier() != 0 || write_pidfile(pidfile) != 0) {
        close(listener);
        return 1;
    }
    if (udp_pidfile_test_ready() != 0) {
        unlink(pidfile);
        close(listener);
        return 1;
    }

    stopping = 0;
    if (signal(SIGTERM, on_signal) == SIG_ERR ||
        signal(SIGINT, on_signal) == SIG_ERR) {
        logerr("UDP relay cannot install signal handlers: %s", strerror(errno));
        unlink(pidfile);
        close(listener);
        return 1;
    }
    struct udp_flow flows[MAX_FLOWS];
    for (int i = 0; i < MAX_FLOWS; i++)
        flows[i].fd = -1;

    int result = 0;
    while (!stopping) {
        struct pollfd pfds[MAX_FLOWS + 1];
        pfds[0] = (struct pollfd){ .fd = listener, .events = POLLIN };
        for (int i = 0; i < MAX_FLOWS; i++)
            pfds[i + 1] = (struct pollfd){ .fd = flows[i].fd,
                                           .events = POLLIN };
        int ready = poll_flows(pfds, MAX_FLOWS + 1);
        if (ready < 0 && errno == EINTR)
            continue;
        if (ready < 0) {
            logerr("UDP relay poll failed: %s", strerror(errno));
            result = 1;
            break;
        }
        if (pfds[0].revents & (POLLERR | POLLHUP | POLLNVAL)) {
            logerr("UDP relay listener reported an unrecoverable poll event");
            result = 1;
            break;
        }

        if (ready > 0 && (pfds[0].revents & POLLIN)) {
            unsigned char buffer[65535];
            struct sockaddr_in client;
            socklen_t client_len = sizeof(client);
            ssize_t n = recvfrom(listener, buffer, sizeof(buffer), 0,
                                 (struct sockaddr *)&client, &client_len);
            if (n >= 0) {
                struct udp_flow *flow = find_flow(flows, &client);
                (void)send_to_target(flow, &client, &target_addr, buffer,
                                     (size_t)n);
            } else if (errno != EINTR && errno != EAGAIN &&
                       errno != EWOULDBLOCK) {
                logerr("UDP relay cannot receive from client: %s",
                       strerror(errno));
                result = 1;
                break;
            }
        }
        for (int i = 0; i < MAX_FLOWS; i++) {
            if (flows[i].fd >= 0 &&
                (pfds[i + 1].revents & (POLLERR | POLLHUP | POLLNVAL))) {
                close_flow(&flows[i]);
                continue;
            }
            if (flows[i].fd >= 0 && (pfds[i + 1].revents & POLLIN)) {
                unsigned char buffer[65535];
                ssize_t n = recv_target(flows[i].fd, buffer, sizeof(buffer));
                if (n < 0) {
                    logerr("UDP relay cannot receive from target: %s",
                           strerror(errno));
                    close_flow(&flows[i]);
                    continue;
                }
                ssize_t sent = sendto(listener, buffer, (size_t)n, 0,
                    (struct sockaddr *)&flows[i].client,
                    sizeof(flows[i].client));
                if (sent != n) {
                    int saved_errno = sent < 0 ? errno : EIO;
                    logerr("UDP relay cannot reply to client: %s",
                           strerror(saved_errno));
                    close_flow(&flows[i]);
                    continue;
                }
                flows[i].active_at = time(NULL);
            }
        }
        time_t now = time(NULL);
        for (int i = 0; i < MAX_FLOWS; i++) {
            if (flows[i].fd >= 0 &&
                now - flows[i].active_at >= FLOW_IDLE_SECONDS)
                close_flow(&flows[i]);
        }
    }

    for (int i = 0; i < MAX_FLOWS; i++)
        close_flow(&flows[i]);
    if (close(listener) != 0) {
        logerr("UDP relay cannot close listener: %s", strerror(errno));
        result = 1;
    }
    if (fs_unlink_if_exists(pidfile) != 0) {
        logerr("UDP relay cannot remove pidfile: %s", strerror(errno));
        result = 1;
    }
    return result;
}
