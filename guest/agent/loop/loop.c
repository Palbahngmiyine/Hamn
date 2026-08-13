#include "loop/loop.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <unistd.h>

#define MAX_FDS 4096
#define MAX_EVENTS 64

struct watcher {
    loop_cb cb;
    void *ud;
};

struct loop {
    int epfd;
    int stopping;
    struct watcher w[MAX_FDS];
};

struct loop *loop_new(void)
{
    struct loop *l = calloc(1, sizeof(*l));
    if (!l)
        return NULL;
    l->epfd = epoll_create1(EPOLL_CLOEXEC);
    if (l->epfd < 0) {
        free(l);
        return NULL;
    }
    return l;
}

static int loop_ctl(struct loop *l, int op, int fd, uint32_t events,
                    loop_cb cb, void *ud)
{
    if (fd < 0 || fd >= MAX_FDS)
        return -1;
    struct epoll_event ev;
    memset(&ev, 0, sizeof(ev));
    ev.events = events;
    ev.data.fd = fd;
    if (epoll_ctl(l->epfd, op, fd, &ev) != 0)
        return -1;
    l->w[fd].cb = cb;
    l->w[fd].ud = ud;
    return 0;
}

int loop_add(struct loop *l, int fd, uint32_t events, loop_cb cb, void *ud)
{
    return loop_ctl(l, EPOLL_CTL_ADD, fd, events, cb, ud);
}

int loop_mod(struct loop *l, int fd, uint32_t events, loop_cb cb, void *ud)
{
    return loop_ctl(l, EPOLL_CTL_MOD, fd, events, cb, ud);
}

int loop_del(struct loop *l, int fd)
{
    if (fd < 0 || fd >= MAX_FDS)
        return -1;
    epoll_ctl(l->epfd, EPOLL_CTL_DEL, fd, NULL);
    l->w[fd].cb = NULL;
    l->w[fd].ud = NULL;
    return 0;
}

int loop_run(struct loop *l)
{
    struct epoll_event evs[MAX_EVENTS];

    while (!l->stopping) {
        int n = epoll_wait(l->epfd, evs, MAX_EVENTS, -1);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        for (int i = 0; i < n; i++) {
            int fd = evs[i].data.fd;
            struct watcher *w = &l->w[fd];
            if (w->cb)
                w->cb(l, fd, evs[i].events, w->ud);
        }
    }
    return 0;
}

void loop_stop(struct loop *l)
{
    l->stopping = 1;
}
