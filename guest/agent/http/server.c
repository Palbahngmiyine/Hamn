#include "http/server.h"

#include <errno.h>
#include <grp.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include "http/conn.h"

static void on_accept(struct loop *l, int fd, uint32_t events, void *ud)
{
    (void)events;
    (void)ud;
    for (;;) {
        int cfd = accept4(fd, NULL, NULL, SOCK_CLOEXEC | SOCK_NONBLOCK);
        if (cfd < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK)
                break;
            if (errno == EINTR)
                continue;
            perror("hamnd: accept");
            break;
        }
        conn_new(l, cfd);
    }
}

int server_listen_unix(struct loop *l, const char *path, const char *group)
{
    if (unlink(path) != 0 && errno != ENOENT) {
        perror("hamnd: unlink stale socket");
        return -1;
    }

    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
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

    if (bind(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0 ||
        listen(fd, 64) != 0) {
        perror("hamnd: bind/listen");
        close(fd);
        return -1;
    }

    struct group *g = group ? getgrnam(group) : NULL;
    if (!g) {
        fprintf(stderr, "hamnd: required group '%s' not found\n",
                group ? group : "(null)");
        close(fd);
        unlink(path);
        return -1;
    }
    if (chown(path, 0, g->gr_gid) != 0 || chmod(path, 0660) != 0) {
        perror("hamnd: socket perms");
        close(fd);
        unlink(path);
        return -1;
    }

    if (loop_add(l, fd, EPOLLIN, on_accept, NULL) != 0) {
        close(fd);
        unlink(path);
        return -1;
    }
    fprintf(stderr, "hamnd: listening on %s\n", path);
    return 0;
}
