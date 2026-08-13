#ifndef HAMN_PROC_H
#define HAMN_PROC_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

typedef int (*proc_completion_fn)(int rc, void *context);

/* argv는 NULL 종단 배열. 종료까지 대기, exit code(0~255) 반환, 실패/시그널 -1. */
int proc_run(const char *const argv[]);

/* 실제 terminal foreground를 command process group에 넘겨 실행한다. */
int proc_run_terminal(const char *const argv[]);

/* stdout을 out(cap)에 캡처(NUL 종단, 끝 개행 제거). exit code 반환. */
int proc_run_capture(const char *const argv[], char *out, size_t cap);

/* 캡처 버퍼가 부족하면 truncated를 1로 설정한다. */
int proc_run_capture_checked(const char *const argv[], char *out, size_t cap,
                             int *truncated);

/*
 * supervisor 내부에서 command를 실행한다. owner가 사라지면 자연 종료를
 * 기다린 뒤 bounded TERM/KILL로 exact child를 reap한다.
 */
int proc_run_guarded(const char *const argv[], char *out, size_t cap,
                     int *truncated, int owner_fd, int release_fd,
                     int spawn_ack_fd);

/*
 * fork supervisor가 상속 lock을 유지하며 exact child를 끝까지 reap한다.
 * caller가 비정상 종료되면 bounded TERM/KILL 정책을 적용한다.
 */
int proc_run_supervised(const char *const argv[]);
int proc_run_supervised_callback(const char *const argv[],
                                 proc_completion_fn completion,
                                 void *context);

/*
 * 새 세션(setsid)으로 분리 spawn. stdin=/dev/null,
 * stdout/stderr는 logfile에 append. 대기하지 않고 pid 반환, 실패 -1.
 */
pid_t proc_spawn_daemon(const char *const argv[], const char *logfile);

/* 현재 실행 파일의 절대 경로. 성공 시 buf 반환, 실패 NULL. */
const char *proc_self_path(char *buf, size_t cap);

int proc_start_identity(pid_t pid, uint64_t *sec, uint64_t *usec);
int proc_executable_identity(pid_t pid, unsigned char uuid[16]);
void proc_executable_uuid_format(const unsigned char uuid[16], char hex[33]);
int proc_pipe_cloexec(int fds[2]);
int proc_write_all(int fd, const void *data, size_t length);
int proc_read_all(int fd, void *data, size_t length);

#endif
