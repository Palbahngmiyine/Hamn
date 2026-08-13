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
int ssh_exec_capture(const struct profile *p, const char *ip,
                     const char *const remote_argv[], char *out, size_t cap);
int ssh_exec_capture_checked(const struct profile *p, const char *ip,
                             const char *const remote_argv[], char *out,
                             size_t cap, int *truncated);

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

#endif
