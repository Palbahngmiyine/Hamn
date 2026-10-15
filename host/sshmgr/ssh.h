#ifndef HAMN_SSH_H
#define HAMN_SSH_H

#include <stddef.h>

#include "core/profile.h"

/*
 * /usr/bin/ssh spawn 기반 SSH 관리.
 * 프로필별 ed25519 키 + ControlMaster 멀티플렉싱(ssh.sock) 사용.
 * 로컬 VM이므로 호스트키 검증은 끈다(known_hosts=/dev/null).
 */

#define SSH_USER "hamn"
/* base 옵션 + 목적지/명령에 충분한 argv 슬롯 */
#define SSH_ARGV_MAX 64
#define SSH_CONTROL_TIMEOUT_MS 5000

/* <profile>/id_ed25519 없으면 ssh-keygen으로 생성. 0=성공 */
int ssh_keys_ensure(const struct profile *p);

/* id_ed25519.pub 한 줄 읽기(개행 제거). 0=성공 */
int ssh_read_pubkey(const struct profile *p, char *buf, size_t cap);

/*
 * 공통 ssh 옵션을 argv[0..]에 채운다 ("ssh"부터). 반환값은 채운 개수.
 * 호출자는 이어서 목적지("hamn@<ip>")와 원격 명령을 추가하고 NULL 종단한다.
 * sb는 옵션 문자열("-i <path>" 등)의 백업 스토리지.
 */
struct ssh_strbuf {
    char key[1024];
    char ctl[1024];
};
int ssh_base_argv(const struct profile *p, const char *argv[], int cap,
                  struct ssh_strbuf *sb);

/* ControlMaster 시작(재시도 포함, timeout_sec 동안). 0=성공 */
int ssh_master_start(const struct profile *p, const char *ip,
                     int timeout_sec);

/* master 살아있으면 0 */
int ssh_master_alive(const struct profile *p);

/* master 종료 (ssh -O exit). */
void ssh_master_exit(const struct profile *p);

/*
 * master를 통해 원격 명령 실행. remote_argv는 NULL 종단.
 * quiet=1이면 stdout 버림. exit code 반환(-1 spawn 실패).
 */
int ssh_exec(const struct profile *p, const char *ip,
             const char *const remote_argv[], int quiet);
/* Quiet bounded command; timeout ends the local client, not guest-side work. */
int ssh_exec_bounded(const struct profile *p, const char *ip,
                     const char *const remote_argv[], unsigned timeout_ms);
/* The same bound, with the command's output left on this process's stdout
 * and stderr, as ssh_exec(..., 0) leaves it. */
int ssh_exec_bounded_shown(const struct profile *p, const char *ip,
                           const char *const remote_argv[],
                           unsigned timeout_ms);
int ssh_exec_capture(const struct profile *p, const char *ip,
                     const char *const remote_argv[], char *out, size_t cap);
/* Bounded like ssh_exec_bounded, with the client's stdout and stderr
 * together in out (cap bytes, NUL-terminated, cut to fit, trailing line
 * ends removed). Returns the exit status, PROC_RUN_TIMEOUT when the
 * deadline ended the client, 130 when cancelled, or -1. */
int ssh_exec_capture_bounded(const struct profile *p, const char *ip,
                             const char *const remote_argv[], char *out,
                             size_t cap, unsigned timeout_ms);
int ssh_exec_capture_checked(const struct profile *p, const char *ip,
                             const char *const remote_argv[], char *out,
                             size_t cap, int *truncated);
/* ssh_exec_capture_bounded that also sets *truncated (when not NULL) to 1
 * when out was too small. A timeout keeps what was captured until then. */
int ssh_exec_capture_bounded_checked(const struct profile *p, const char *ip,
                                     const char *const remote_argv[],
                                     char *out, size_t cap, int *truncated,
                                     unsigned timeout_ms);

/*
 * A guest command that the guest itself bounds runs under
 * `timeout SSH_REMOTE_KILL_AFTER <limit>`, which ends it within
 * limit + SSH_REMOTE_KILL_AFTER_SEC seconds of its start. Its client waits
 * SSH_REMOTE_MARGIN_SEC more for the dispatch through the master and the exit
 * status to come back, and is reaped at SSH_REMOTE_CLIENT_MS(limit). A client
 * reaped there leaves the command's outcome unknown: the guest may not have
 * answered, or the connection hung. limit_sec is at most a few hours. Tests
 * build with a smaller margin.
 */
#define SSH_REMOTE_KILL_AFTER "--kill-after=5s"
#define SSH_REMOTE_KILL_AFTER_SEC 5U
#ifndef SSH_REMOTE_MARGIN_SEC
#define SSH_REMOTE_MARGIN_SEC 10U
#endif
#define SSH_REMOTE_CLIENT_MS(limit_sec) \
    (((unsigned)(limit_sec) + SSH_REMOTE_KILL_AFTER_SEC + \
      SSH_REMOTE_MARGIN_SEC) * 1000U)

/* ssh_forward.c — control master에 unix socket 포워드 추가/해제 */
int ssh_forward_add_unix(const struct profile *p, const char *ip,
                         const char *local_sock, const char *remote_sock);
int ssh_forward_cancel_unix(const struct profile *p, const char *ip,
                            const char *local_sock,
                            const char *remote_sock);
int ssh_forward_add_tcp(const struct profile *p, const char *ip,
                        const char *bind_address, unsigned local_port,
                        const char *remote_address, unsigned remote_port);
typedef int (*ssh_forward_completion_fn)(int rc, void *context);
int ssh_forward_add_tcp_observed(const struct profile *p, const char *ip,
                                 const char *bind_address,
                                 unsigned local_port,
                                 const char *remote_address,
                                 unsigned remote_port,
                                 ssh_forward_completion_fn completion,
                                 void *context);
int ssh_forward_cancel_tcp(const struct profile *p, const char *ip,
                           const char *bind_address, unsigned local_port,
                           const char *remote_address, unsigned remote_port);
/* TCP forward requests and all cancel requests print nothing. These are the
 * ssh client's one-line output for the most recent such request that failed,
 * and "" after one that succeeded: static strings, valid until the next
 * request of that kind in this single-threaded process. A caller reports one
 * when it reports the failure, and not for an outcome it expects. */
const char *ssh_forward_add_detail(void);
const char *ssh_forward_cancel_detail(void);

#endif
