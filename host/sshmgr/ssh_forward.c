#include <stdio.h>
#include <unistd.h>

#include "sshmgr/ssh.h"
#include "util/proc.h"

static int run_forward_command(const char *const argv[],
                               ssh_forward_completion_fn completion,
                               void *context)
{
    return completion ?
        proc_run_supervised_callback(argv, completion, context) :
        proc_run_supervised(argv);
}

static int forward_ctl(const struct profile *p, const char *ip,
                       const char *op, const char *spec,
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

    return run_forward_command(argv, completion, context);
}

int ssh_forward_add_unix(const struct profile *p, const char *ip,
                         const char *local_sock, const char *remote_sock)
{
    char spec[2100];
    snprintf(spec, sizeof(spec), "%s:%s", local_sock, remote_sock);
    unlink(local_sock); /* 이전 실행이 남긴 소켓 파일 제거 */
    return forward_ctl(p, ip, "forward", spec, NULL, NULL);
}

int ssh_forward_cancel_unix(const struct profile *p, const char *ip,
                            const char *local_sock, const char *remote_sock)
{
    char spec[2100];
    snprintf(spec, sizeof(spec), "%s:%s", local_sock, remote_sock);
    int rc = forward_ctl(p, ip, "cancel", spec, NULL, NULL);
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
    return forward_ctl(p, ip, op, spec, completion, context);
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
