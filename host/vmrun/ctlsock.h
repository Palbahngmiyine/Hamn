#ifndef HAMN_CTLSOCK_H
#define HAMN_CTLSOCK_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

/*
 * vmrun 제어 소켓 — JSON 한 줄 요청/응답 프로토콜.
 *   {"cmd":"status"} →
 *     {"state":"running","pid":123,"start_sec":1,"start_usec":2}
 *   {"cmd":"stop"}   → {"ok":true}  (graceful stop 시퀀스 개시)
 */

typedef struct vz_vm vz_vm;
typedef void (*ctl_stop_fn)(void);

/* How many accepted connections may wait for their request at once. */
#define CTLSOCK_WAITING_MAX 16

/* How many connections one status query opens at most. */
#define CTLSOCK_STATUS_ATTEMPTS 3

enum ctlsock_query_result {
    CTLSOCK_QUERY_OK = 0,
    CTLSOCK_QUERY_UNAVAILABLE = -1,
    CTLSOCK_QUERY_UNCERTAIN = -2,
};

/*
 * vmrun 쪽: 수신 소켓 개설(0600). dispatch 메인 큐에서 서빙. 0=성공
 *
 * A connection is answered once, when its request is complete: at the first
 * newline, at the peer's end of input, or at 511 bytes. The request may
 * arrive any time after the connection is accepted and in any number of
 * pieces; the server waits for it without blocking the main queue and without
 * a deadline. A peer that closes without sending anything is forgotten. What
 * a peer sent before it closed is still carried out, without a reply. When
 * CTLSOCK_WAITING_MAX connections already wait, a further one closes the one
 * that has waited longest, so a peer that never sends holds one descriptor
 * at most until that many others have connected.
 */
int ctlsock_serve(const char *path, vz_vm *vm, uint64_t start_sec,
                  uint64_t start_usec, ctl_stop_fn on_stop,
                  dev_t *dev_out, ino_t *ino_out);

/* bind 직후 캡처한 filesystem socket identity와 같을 때만 path 제거. */
int ctlsock_unlink_owned(const char *path, dev_t dev, ino_t ino);

/*
 * 클라이언트 쪽: status 요청 한 줄 전송 후 응답 한 줄 수신.
 * OK=resp 채움, UNAVAILABLE=연결 전 실패,
 * UNCERTAIN=연결된 제어 소켓의 IO 실패.
 *
 * resp receives at most cap - 1 bytes and a terminating NUL. timeout_ms
 * bounds each send and each receive. A supervisor of Hamn 0.2.1 or earlier
 * closes a connection it accepts before the request has arrived. A status
 * request changes nothing in the supervisor, so when the server closes a
 * connection without replying the request is sent again on a new one, on at
 * most CTLSOCK_STATUS_ATTEMPTS connections in all; UNCERTAIN if none is
 * answered. A timeout or any other failure is reported at once.
 */
int ctlsock_query_status(const char *path, char *resp, size_t cap,
                         int timeout_ms);

#ifdef HAMN_TEST
void ctlsock_test_fail_nonblocking_once(int error);
/* Runs hook in every status attempt after it connects and before it sends. */
void ctlsock_test_before_send(void (*hook)(void));
#endif

#endif
