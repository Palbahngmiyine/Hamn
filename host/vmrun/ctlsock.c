#include "vmrun/ctlsock.h"

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include <dispatch/dispatch.h>

#include "vz/vz_shim.h"

#ifdef HAMN_TEST
static int test_nonblocking_error;

static void (*test_before_send)(void);

void ctlsock_test_fail_nonblocking_once(int error)
{
    test_nonblocking_error = error;
}

void ctlsock_test_before_send(void (*hook)(void))
{
    test_before_send = hook;
}
#endif

#define CTL_REQUEST_CAP 512

/* An accepted connection whose request line has not fully arrived. */
struct ctl_conn {
    int fd;                   /* -1 marks a free slot */
    uint64_t order;           /* accept order: the smallest waited longest */
    dispatch_source_t source; /* reads fd; its cancel handler closes fd */
    int peer_gone;            /* closed before the accept: send no reply */
    size_t length;
    char request[CTL_REQUEST_CAP];
};

/* One served socket. Only the dispatch main queue reads or writes it. */
struct ctl_server {
    vz_vm *vm;
    uint64_t start_sec;
    uint64_t start_usec;
    ctl_stop_fn on_stop;
    uint64_t accepted;
    struct ctl_conn waiting[CTLSOCK_WAITING_MAX];
};

static const char *state_name(enum vz_state st)
{
    switch (st) {
    case VZ_ST_STOPPED:  return "stopped";
    case VZ_ST_RUNNING:  return "running";
    case VZ_ST_PAUSED:   return "paused";
    case VZ_ST_ERROR:    return "error";
    case VZ_ST_STARTING: return "starting";
    case VZ_ST_STOPPING: return "stopping";
    default:             return "unknown";
    }
}

/* Ends a waiting connection and frees its slot. The descriptor stays open
 * until the cancel handler runs, so its number cannot name a newer connection
 * before then, and a cancelled source delivers no further event, so the slot
 * can hold another connection at once. */
static void conn_end(struct ctl_conn *conn)
{
    dispatch_source_cancel(conn->source);
    dispatch_release(conn->source);
    conn->source = NULL;
    conn->fd = -1;
}

/* Reads what has arrived on a waiting connection and answers it once its
 * request is complete. An accepted connection carries no request yet when
 * its peer is still between connect() and write(), so the read source, not
 * the accept, decides when to read. */
static void conn_readable(struct ctl_server *server, struct ctl_conn *conn)
{
    ssize_t n = read(conn->fd, conn->request + conn->length,
                     sizeof(conn->request) - 1 - conn->length);
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR))
        return;
    if (n < 0 || (n == 0 && conn->length == 0)) {
        conn_end(conn);
        return;
    }
    conn->length += (size_t)n;
    conn->request[conn->length] = '\0';
    /* A request is one line. A peer that ended its input or filled the
     * buffer has sent all of the request that is read. */
    if (n > 0 && conn->length < sizeof(conn->request) - 1 &&
        !memchr(conn->request, '\n', conn->length))
        return;

    char resp[256];
    int do_stop = 0;
    if (strstr(conn->request, "\"status\"")) {
        snprintf(resp, sizeof(resp),
                 "{\"state\":\"%s\",\"pid\":%d,"
                 "\"start_sec\":%" PRIu64 ",\"start_usec\":%" PRIu64
                 "}\n",
                 state_name(vz_vm_state(server->vm)), getpid(),
                 server->start_sec, server->start_usec);
    } else if (strstr(conn->request, "\"stop\"")) {
        snprintf(resp, sizeof(resp), "{\"ok\":true}\n");
        do_stop = 1;
    } else {
        snprintf(resp, sizeof(resp), "{\"error\":\"unknown command\"}\n");
    }
    /* A peer that goes away now gets no reply; nobody is left to tell. */
    if (!conn->peer_gone)
        write(conn->fd, resp, strlen(resp));
    conn_end(conn);

    if (do_stop && server->on_stop)
        server->on_stop();
}

/* Starts waiting for the request of a newly accepted connection. */
static void conn_admit(struct ctl_server *server, int cfd)
{
    /* Reading must never block the main queue. */
    int flags = fcntl(cfd, F_GETFL);
    if (flags < 0 || fcntl(cfd, F_SETFL, flags | O_NONBLOCK) != 0) {
        close(cfd);
        return;
    }
    /* A reply to a peer that has gone away must not raise SIGPIPE in the
     * supervisor. The kernel refuses the option once the peer has closed;
     * the request such a peer sent first is still carried out, without a
     * reply. */
    int one = 1;
    int peer_gone = setsockopt(cfd, SOL_SOCKET, SO_NOSIGPIPE, &one,
                               sizeof(one)) != 0;
    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_READ, (uintptr_t)cfd, 0,
        dispatch_get_main_queue());
    if (!source) {
        close(cfd);
        return;
    }

    /* A free slot, or else the connection that has waited longest. */
    struct ctl_conn *conn = &server->waiting[0];
    for (size_t i = 0; i < CTLSOCK_WAITING_MAX; i++) {
        struct ctl_conn *candidate = &server->waiting[i];
        if (candidate->fd < 0) {
            conn = candidate;
            break;
        }
        if (candidate->order < conn->order)
            conn = candidate;
    }
    if (conn->fd >= 0)
        conn_end(conn);

    conn->fd = cfd;
    conn->order = ++server->accepted;
    conn->source = source;
    conn->peer_gone = peer_gone;
    conn->length = 0;
    dispatch_source_set_event_handler(source, ^{
        conn_readable(server, conn);
    });
    dispatch_source_set_cancel_handler(source, ^{
        close(cfd);
    });
    dispatch_resume(source);
}

static int socket_path_capture(const char *path, dev_t *dev_out,
                               ino_t *ino_out)
{
    struct stat sb;
    if (!path || !dev_out || !ino_out || lstat(path, &sb) != 0 ||
        !S_ISSOCK(sb.st_mode) || sb.st_uid != geteuid() || sb.st_nlink != 1)
        return -1;
    *dev_out = sb.st_dev;
    *ino_out = sb.st_ino;
    return 0;
}

int ctlsock_unlink_owned(const char *path, dev_t dev, ino_t ino)
{
    struct stat sb;
    if (!path) {
        errno = EINVAL;
        return -1;
    }
    if (lstat(path, &sb) != 0)
        return errno == ENOENT ? 0 : -1;
    if (!S_ISSOCK(sb.st_mode) || sb.st_dev != dev || sb.st_ino != ino) {
        errno = ESTALE;
        return -1;
    }
    return unlink(path);
}

int ctlsock_serve(const char *path, vz_vm *vm, uint64_t start_sec,
                  uint64_t start_usec, ctl_stop_fn on_stop,
                  dev_t *dev_out, ino_t *ino_out)
{
    if (dev_out)
        *dev_out = 0;
    if (ino_out)
        *ino_out = 0;

    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;

    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    if (strlen(path) >= sizeof(sa.sun_path)) {
        close(fd);
        return -1;
    }
    strcpy(sa.sun_path, path);

    if (bind(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0) {
        close(fd);
        return -1;
    }
    dev_t path_dev;
    ino_t path_ino;
    if (socket_path_capture(path, &path_dev, &path_ino) != 0) {
        close(fd);
        return -1;
    }
    if (chmod(path, 0600) != 0 || listen(fd, 8) != 0) {
        int saved = errno;
        (void)ctlsock_unlink_owned(path, path_dev, path_ino);
        close(fd);
        errno = saved;
        return -1;
    }
    int flags = fcntl(fd, F_GETFL);
#ifdef HAMN_TEST
    if (test_nonblocking_error != 0) {
        errno = test_nonblocking_error;
        test_nonblocking_error = 0;
        flags = -1;
    }
#endif
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) {
        int saved = errno;
        (void)ctlsock_unlink_owned(path, path_dev, path_ino);
        close(fd);
        errno = saved;
        return -1;
    }

    dev_t current_dev;
    ino_t current_ino;
    if (socket_path_capture(path, &current_dev, &current_ino) != 0 ||
        current_dev != path_dev || current_ino != path_ino) {
        (void)ctlsock_unlink_owned(path, path_dev, path_ino);
        close(fd);
        errno = ESTALE;
        return -1;
    }

    struct ctl_server *server = calloc(1, sizeof(*server));
    dispatch_source_t src = server ? dispatch_source_create(
        DISPATCH_SOURCE_TYPE_READ, (uintptr_t)fd, 0,
        dispatch_get_main_queue()) : NULL;
    if (!src) {
        int saved = errno;
        free(server);
        (void)ctlsock_unlink_owned(path, path_dev, path_ino);
        close(fd);
        errno = saved;
        return -1;
    }
    server->vm = vm;
    server->start_sec = start_sec;
    server->start_usec = start_usec;
    server->on_stop = on_stop;
    for (size_t i = 0; i < CTLSOCK_WAITING_MAX; i++)
        server->waiting[i].fd = -1;
    dispatch_source_set_event_handler(src, ^{
        for (;;) {
            int cfd = accept(fd, NULL, NULL);
            if (cfd < 0) {
                if (errno == EWOULDBLOCK || errno == EAGAIN)
                    break;
                if (errno == EINTR)
                    continue;
                break;
            }
            conn_admit(server, cfd);
        }
    });
    dispatch_resume(src);
    if (dev_out)
        *dev_out = path_dev;
    if (ino_out)
        *ino_out = path_ino;
    /* 소켓, source, server는 vmrun 수명 동안 유지 (의도적 누수) */
    return 0;
}

/* What one connection of a status query came to. */
enum status_attempt {
    ATTEMPT_REPLIED,
    ATTEMPT_UNAVAILABLE,
    ATTEMPT_UNCERTAIN,
    /* The server closed the connection without replying. */
    ATTEMPT_UNANSWERED,
};

static enum status_attempt status_attempt(const char *path, char *resp,
                                          size_t cap, int timeout_ms)
{
    static const char request[] = "{\"cmd\":\"status\"}\n";
    const ssize_t len = (ssize_t)sizeof(request) - 1;

    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return ATTEMPT_UNCERTAIN;

    struct timeval tv = { .tv_sec = timeout_ms / 1000,
                          .tv_usec = (timeout_ms % 1000) * 1000 };
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));

    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    if (strlen(path) >= sizeof(sa.sun_path)) {
        close(fd);
        return ATTEMPT_UNCERTAIN;
    }
    strcpy(sa.sun_path, path);

    if (connect(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0) {
        int saved = errno;
        close(fd);
        return saved == ENOENT || saved == ECONNREFUSED ||
                       saved == ENOTSOCK
                   ? ATTEMPT_UNAVAILABLE
                   : ATTEMPT_UNCERTAIN;
    }
#ifdef HAMN_TEST
    if (test_before_send)
        test_before_send();
#endif
    /* A server that closed the connection before the request reached it
     * fails the write with EPIPE; one that closed it afterwards ends the
     * reply before its first byte. */
    ssize_t sent = write(fd, request, (size_t)len);
    if (sent != len) {
        int saved = errno;
        close(fd);
        return sent < 0 && saved == EPIPE ? ATTEMPT_UNANSWERED
                                          : ATTEMPT_UNCERTAIN;
    }
    ssize_t n = read(fd, resp, cap - 1);
    close(fd);
    if (n == 0)
        return ATTEMPT_UNANSWERED;
    if (n < 0)
        return ATTEMPT_UNCERTAIN;
    resp[n] = '\0';
    return ATTEMPT_REPLIED;
}

int ctlsock_query_status(const char *path, char *resp, size_t cap,
                         int timeout_ms)
{
    for (int attempt = 0; attempt < CTLSOCK_STATUS_ATTEMPTS; attempt++) {
        switch (status_attempt(path, resp, cap, timeout_ms)) {
        case ATTEMPT_REPLIED:
            return CTLSOCK_QUERY_OK;
        case ATTEMPT_UNAVAILABLE:
            return CTLSOCK_QUERY_UNAVAILABLE;
        case ATTEMPT_UNCERTAIN:
            return CTLSOCK_QUERY_UNCERTAIN;
        case ATTEMPT_UNANSWERED:
            break;
        }
    }
    return CTLSOCK_QUERY_UNCERTAIN;
}
