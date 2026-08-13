#ifndef HAMND_CONN_H
#define HAMND_CONN_H

#include <stddef.h>

#include "loop/loop.h"

struct conn;

struct conn *conn_new(struct loop *l, int fd);
void conn_close(struct conn *c);

/* 전체 기록 보장 (POLLOUT 대기 포함). 0=성공 */
int conn_write(struct conn *c, const void *buf, size_t n);
void conn_isolate_worker(struct conn *c, int slot_fd);
/* timeout_ms 동안 client 입력/EOF를 기다림. 1=종료, 0=연결 유지, -1=오류. */
int conn_wait_client_closed(struct conn *c, int timeout_ms);
/* fork된 응답 작업자에게 fd를 넘긴 뒤 부모 event loop에서 연결을 분리한다. */
void conn_handoff(struct conn *c);

#endif
