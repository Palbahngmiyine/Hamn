#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "sshmgr/ssh.h"
#include "util/proc.h"

/* What the ssh client printed for the most recent TCP forward request and
 * the most recent cancel request that failed. */
static char add_detail[512];
static char cancel_detail[512];

const char *ssh_forward_add_detail(void)
{
    return add_detail;
}

const char *ssh_forward_cancel_detail(void)
{
    return cancel_detail;
}

/* Runs a control request whose failure the caller reports itself: the
 * client's text is captured into detail as one line instead of reaching the
 * user's stderr or the observer's log once per retry. */
static int run_captured(const char *const argv[], char detail[512],
                        ssh_forward_completion_fn completion, void *context)
{
    memset(detail, 0, 512);
    int rc = proc_run_bounded(argv, detail, 512, SSH_CONTROL_TIMEOUT_MS,
                              completion, context);
    detail[511] = '\0';
    if (rc == 0) {
        detail[0] = '\0';
        return 0;
    }
    /* The client ends each message with CR LF; keep one line. */
    size_t length = strlen(detail);
    while (length && (detail[length - 1] == '\n' || detail[length - 1] == '\r'))
        detail[--length] = '\0';
    for (char *byte = detail; *byte; byte++) {
        if (*byte == '\n' || *byte == '\r')
            *byte = ' ';
    }
    return rc;
}

/* detail is where a failed request's client output goes, or NULL to let the
 * client write to this process's stderr. */
static int forward_ctl(const struct profile *p, const char *ip,
                       const char *op, const char *spec, char *detail,
                       ssh_forward_completion_fn completion, void *context)
{
    const char *argv[SSH_ARGV_MAX];
    struct ssh_strbuf sb;
    char dest[128];
    int n = ssh_base_argv(p, argv, SSH_ARGV_MAX - 6, &sb);
    if (n < 0)
        return -1;

    snprintf(dest, sizeof(dest), "%s@%s", SSH_USER, ip);
    argv[n++] = "-O";
    argv[n++] = op;
    argv[n++] = "-L";
    argv[n++] = spec;
    argv[n++] = dest;
    argv[n] = NULL;

    return detail ? run_captured(argv, detail, completion, context) :
        proc_run_bounded(argv, NULL, 0, SSH_CONTROL_TIMEOUT_MS, completion,
                         context);
}

int ssh_forward_add_unix(const struct profile *p, const char *ip,
                         const char *local_sock, const char *remote_sock)
{
    char spec[2100];
    snprintf(spec, sizeof(spec), "%s:%s", local_sock, remote_sock);
    unlink(local_sock); /* 이전 실행이 남긴 소켓 파일 제거 */
    /* A failed socket forward ends the start, which shows the client's text. */
    return forward_ctl(p, ip, "forward", spec, NULL, NULL, NULL);
}

int ssh_forward_cancel_unix(const struct profile *p, const char *ip,
                            const char *local_sock, const char *remote_sock)
{
    char spec[2100];
    snprintf(spec, sizeof(spec), "%s:%s", local_sock, remote_sock);
    /* Cancelling a forward that does not exist, or after the master ended
     * with the guest, is an outcome callers expect. */
    int rc = forward_ctl(p, ip, "cancel", spec, cancel_detail, NULL, NULL);
    unlink(local_sock);
    return rc;
}

static int forward_tcp(const struct profile *p, const char *ip,
                       const char *op, const char *bind_address,
                       unsigned local_port, const char *remote_address,
                       unsigned remote_port,
                       ssh_forward_completion_fn completion, void *context)
{
    char spec[256];
    int n = snprintf(spec, sizeof(spec), "%s:%u:%s:%u", bind_address,
                     local_port, remote_address, remote_port);
    if (n < 0 || n >= (int)sizeof(spec))
        return -1;
    return forward_ctl(p, ip, op, spec,
                       strcmp(op, "cancel") == 0 ? cancel_detail : add_detail,
                       completion, context);
}

int ssh_forward_add_tcp(const struct profile *p, const char *ip,
                        const char *bind_address, unsigned local_port,
                        const char *remote_address, unsigned remote_port)
{
    return forward_tcp(p, ip, "forward", bind_address, local_port,
                       remote_address, remote_port, NULL, NULL);
}

int ssh_forward_add_tcp_observed(const struct profile *p, const char *ip,
                                 const char *bind_address,
                                 unsigned local_port,
                                 const char *remote_address,
                                 unsigned remote_port,
                                 ssh_forward_completion_fn completion,
                                 void *context)
{
    return forward_tcp(p, ip, "forward", bind_address, local_port,
                       remote_address, remote_port, completion, context);
}

int ssh_forward_cancel_tcp(const struct profile *p, const char *ip,
                           const char *bind_address, unsigned local_port,
                           const char *remote_address, unsigned remote_port)
{
    return forward_tcp(p, ip, "cancel", bind_address, local_port,
                       remote_address, remote_port, NULL, NULL);
}
