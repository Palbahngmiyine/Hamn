#include "vmrun/ctlsock.h"

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include <dispatch/dispatch.h>

#include "vz/vz_shim.h"

#ifdef HAMN_TEST
static int test_nonblocking_error;

void ctlsock_test_fail_nonblocking_once(int error)
{
    test_nonblocking_error = error;
}
#endif

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

static void handle_conn(int cfd, vz_vm *vm, uint64_t start_sec,
                        uint64_t start_usec, ctl_stop_fn on_stop)
{
    char req[512];
    ssize_t n = read(cfd, req, sizeof(req) - 1);
    if (n <= 0) {
        close(cfd);
        return;
    }
    req[n] = '\0';

    char resp[256];
    int do_stop = 0;
    if (strstr(req, "\"status\"")) {
        snprintf(resp, sizeof(resp),
                 "{\"state\":\"%s\",\"pid\":%d,"
                 "\"start_sec\":%" PRIu64 ",\"start_usec\":%" PRIu64
                 "}\n",
                 state_name(vz_vm_state(vm)), getpid(), start_sec,
                 start_usec);
    } else if (strstr(req, "\"stop\"")) {
        snprintf(resp, sizeof(resp), "{\"ok\":true}\n");
        do_stop = 1;
    } else {
        snprintf(resp, sizeof(resp), "{\"error\":\"unknown command\"}\n");
    }
    write(cfd, resp, strlen(resp));
    close(cfd);

    if (do_stop && on_stop)
        on_stop();
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

    dispatch_source_t src = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_READ, (uintptr_t)fd, 0,
        dispatch_get_main_queue());
    if (!src) {
        int saved = errno;
        (void)ctlsock_unlink_owned(path, path_dev, path_ino);
        close(fd);
        errno = saved;
        return -1;
    }
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
            int one = 1;
            setsockopt(cfd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
            handle_conn(cfd, vm, start_sec, start_usec, on_stop);
        }
    });
    dispatch_resume(src);
    if (dev_out)
        *dev_out = path_dev;
    if (ino_out)
        *ino_out = path_ino;
    /* 소켓과 source는 vmrun 수명 동안 유지 (의도적 누수) */
    return 0;
}

int ctlsock_query(const char *path, const char *req, char *resp, size_t cap,
                  int timeout_ms)
{
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return CTLSOCK_QUERY_UNCERTAIN;

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
        return CTLSOCK_QUERY_UNCERTAIN;
    }
    strcpy(sa.sun_path, path);

    if (connect(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0) {
        int saved = errno;
        close(fd);
        return saved == ENOENT || saved == ECONNREFUSED ||
                       saved == ENOTSOCK
                   ? CTLSOCK_QUERY_UNAVAILABLE
                   : CTLSOCK_QUERY_UNCERTAIN;
    }
    char line[600];
    int len = snprintf(line, sizeof(line), "%s\n", req);
    if (len <= 0 || len >= (int)sizeof(line)) {
        close(fd);
        return CTLSOCK_QUERY_UNCERTAIN;
    }
    if (write(fd, line, (size_t)len) != len) {
        close(fd);
        return CTLSOCK_QUERY_UNCERTAIN;
    }
    ssize_t n = read(fd, resp, cap - 1);
    close(fd);
    if (n <= 0)
        return CTLSOCK_QUERY_UNCERTAIN;
    resp[n] = '\0';
    return CTLSOCK_QUERY_OK;
}
