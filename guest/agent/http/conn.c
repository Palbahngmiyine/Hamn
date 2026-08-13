#include "http/conn.h"

#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>

#include "api/router.h"
#include "http/http.h"

#define RDBUF_INIT (16 * 1024)
#define RDBUF_MAX  (1 * 1024 * 1024) /* 헤더+바디 수신 상한 (M2 기준) */

struct conn {
    int fd;
    struct loop *l;
    char *buf;
    size_t cap, len;
    size_t prev_len; /* picohttpparser 증분 파싱용 */
    int handed_off;
};

const char *http_req_header(const struct http_req *r, const char *name,
                            size_t *len)
{
    size_t nlen = strlen(name);
    for (size_t i = 0; i < r->nheaders; i++) {
        if (r->headers[i].name_len == nlen &&
            strncasecmp(r->headers[i].name, name, nlen) == 0) {
            if (len)
                *len = r->headers[i].value_len;
            return r->headers[i].value;
        }
    }
    return NULL;
}

int http_req_query(const struct http_req *r, const char *key, char *out,
                   size_t cap)
{
    if (cap == 0)
        return -1;
    size_t klen = strlen(key);
    const char *q = r->query;
    while (*q) {
        const char *eq = strchr(q, '=');
        const char *amp = strchr(q, '&');
        const char *end = amp ? amp : q + strlen(q);
        if (eq && eq < end && (size_t)(eq - q) == klen &&
            strncmp(q, key, klen) == 0) {
            size_t vlen = (size_t)(end - eq - 1);
            if (vlen >= cap)
                return -1;
            memcpy(out, eq + 1, vlen);
            out[vlen] = '\0';
            return 0;
        }
        if (!amp)
            break;
        q = amp + 1;
    }
    return -1;
}

static int parse_content_length(const struct http_req *r, size_t *out)
{
    int found = 0;
    size_t value = 0;
    for (size_t i = 0; i < r->nheaders; i++) {
        const struct phr_header *header = &r->headers[i];
        if (header->name_len != strlen("Content-Length") ||
            strncasecmp(header->name, "Content-Length",
                        header->name_len) != 0)
            continue;
        if (found)
            return -1;
        found = 1;
        const char *p = header->value;
        size_t len = header->value_len;
        while (len > 0 && (*p == ' ' || *p == '\t')) {
            p++;
            len--;
        }
        while (len > 0 && (p[len - 1] == ' ' || p[len - 1] == '\t'))
            len--;
        if (len == 0)
            return -1;
        for (size_t j = 0; j < len; j++) {
            unsigned char digit = (unsigned char)p[j];
            if (digit < '0' || digit > '9' ||
                value > (RDBUF_MAX - (size_t)(digit - '0')) / 10)
                return -1;
            value = value * 10 + (size_t)(digit - '0');
        }
    }
    *out = value;
    return 0;
}

static int reject_bad_request(struct conn *c)
{
    static const char bad[] =
        "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
    (void)conn_write(c, bad, sizeof(bad) - 1);
    conn_close(c);
    return -1;
}

void conn_close(struct conn *c)
{
    loop_del(c->l, c->fd);
    close(c->fd);
    free(c->buf);
    free(c);
}

int conn_write(struct conn *c, const void *buf, size_t n)
{
    const char *p = buf;
    while (n > 0) {
        ssize_t w = write(c->fd, p, n);
        if (w < 0) {
            if (errno == EINTR)
                continue;
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                struct pollfd pf = { .fd = c->fd, .events = POLLOUT };
                int ready;
                do {
                    ready = poll(&pf, 1, 5000);
                } while (ready < 0 && errno == EINTR);
                if (ready == 0)
                    errno = ETIMEDOUT;
                if (ready <= 0 ||
                    (pf.revents & (POLLERR | POLLHUP | POLLNVAL)))
                    return -1;
                continue;
            }
            return -1;
        }
        p += w;
        n -= (size_t)w;
    }
    return 0;
}

void conn_isolate_worker(struct conn *c, int slot_fd)
{
    long maxfd = sysconf(_SC_OPEN_MAX);
    if (maxfd < 0 || maxfd > 65536)
        maxfd = 65536;
    for (int fd = 3; fd < maxfd; fd++) {
        if (fd != c->fd && fd != slot_fd)
            close(fd);
    }
}

int conn_wait_client_closed(struct conn *c, int timeout_ms)
{
    struct pollfd descriptor = { .fd = c->fd, .events = POLLIN };
    int ready;
    do {
        ready = poll(&descriptor, 1, timeout_ms);
    } while (ready < 0 && errno == EINTR);
    if (ready <= 0)
        return ready;
    return descriptor.revents &
        (POLLIN | POLLERR | POLLHUP | POLLNVAL) ? 1 : 0;
}

void conn_handoff(struct conn *c)
{
    c->handed_off = 1;
}

/* 0=요청 하나 처리함, 1=더 읽어야 함, -1=커넥션 종료됨 */
static int try_dispatch(struct conn *c)
{
    const char *method, *path;
    size_t mlen, plen;
    int minor;
    struct phr_header headers[HTTP_MAX_HEADERS];
    size_t nheaders = HTTP_MAX_HEADERS;

    int hdrlen = phr_parse_request(c->buf, c->len, &method, &mlen, &path,
                                   &plen, &minor, headers, &nheaders,
                                   c->prev_len);
    c->prev_len = c->len;
    if (hdrlen == -2)
        return 1; /* incomplete */
    if (hdrlen == -1)
        return reject_bad_request(c);

    struct http_req r;
    memset(&r, 0, sizeof(r));
    if (mlen >= sizeof(r.method) || plen >= sizeof(r.path)) {
        conn_close(c);
        return -1;
    }
    memcpy(r.method, method, mlen);
    memcpy(r.path, path, plen);
    r.path[plen] = '\0';
    r.minor_version = minor;
    memcpy(r.headers, headers, sizeof(headers[0]) * nheaders);
    r.nheaders = nheaders;

    char *qm = strchr(r.path, '?');
    if (qm) {
        *qm = '\0';
        if (strlen(qm + 1) >= sizeof(r.query))
            return reject_bad_request(c);
        snprintf(r.query, sizeof(r.query), "%s", qm + 1);
    }

    /* 바디: Content-Length만 지원 (chunked 요청은 추후 마일스톤) */
    size_t clen = 0;
    size_t vlen;
    if (parse_content_length(&r, &clen) != 0 ||
        clen > RDBUF_MAX - (size_t)hdrlen ||
        http_req_header(&r, "Transfer-Encoding", NULL) != NULL)
        return reject_bad_request(c);
    if (c->len < (size_t)hdrlen + clen)
        return 1; /* 바디 대기 */
    r.body = c->buf + hdrlen;
    r.body_len = clen;

    r.keep_alive = 1;
    const char *conn_hdr = http_req_header(&r, "Connection", &vlen);
    if (conn_hdr && vlen == 5 && strncasecmp(conn_hdr, "close", 5) == 0)
        r.keep_alive = 0;

    router_dispatch(c, &r);

    if (c->handed_off) {
        loop_del(c->l, c->fd);
        close(c->fd);
        free(c->buf);
        free(c);
        return -1;
    }

    if (!r.keep_alive) {
        conn_close(c);
        return -1;
    }
    /* 다음 요청 준비 (파이프라이닝은 미지원 — 남은 바이트 이동) */
    size_t used = (size_t)hdrlen + clen;
    memmove(c->buf, c->buf + used, c->len - used);
    c->len -= used;
    c->prev_len = 0;
    return 0;
}

static void on_readable(struct loop *l, int fd, uint32_t events, void *ud)
{
    (void)l;
    (void)fd;
    (void)events;
    struct conn *c = ud;

    for (;;) {
        if (c->len == c->cap) {
            if (c->cap >= RDBUF_MAX) {
                conn_close(c);
                return;
            }
            c->cap *= 2;
            c->buf = realloc(c->buf, c->cap);
        }
        ssize_t n = read(c->fd, c->buf + c->len, c->cap - c->len);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            if (errno == EAGAIN || errno == EWOULDBLOCK)
                break;
            conn_close(c);
            return;
        }
        if (n == 0) {
            conn_close(c);
            return;
        }
        c->len += (size_t)n;

        int rc;
        do {
            rc = try_dispatch(c);
        } while (rc == 0 && c->len > 0);
        if (rc == -1)
            return; /* 커넥션 닫힘 */
    }
}

struct conn *conn_new(struct loop *l, int fd)
{
    struct conn *c = calloc(1, sizeof(*c));
    if (!c)
        return NULL;
    c->fd = fd;
    c->l = l;
    c->cap = RDBUF_INIT;
    c->buf = malloc(c->cap);
    if (!c->buf || loop_add(l, fd, EPOLLIN, on_readable, c) != 0) {
        free(c->buf);
        free(c);
        close(fd);
        return NULL;
    }
    return c;
}
