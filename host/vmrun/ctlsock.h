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

enum ctlsock_query_result {
    CTLSOCK_QUERY_OK = 0,
    CTLSOCK_QUERY_UNAVAILABLE = -1,
    CTLSOCK_QUERY_UNCERTAIN = -2,
};

/* vmrun 쪽: 수신 소켓 개설(0600). dispatch 메인 큐에서 서빙. 0=성공 */
int ctlsock_serve(const char *path, vz_vm *vm, uint64_t start_sec,
                  uint64_t start_usec, ctl_stop_fn on_stop,
                  dev_t *dev_out, ino_t *ino_out);

/* bind 직후 캡처한 filesystem socket identity와 같을 때만 path 제거. */
int ctlsock_unlink_owned(const char *path, dev_t dev, ino_t ino);

/*
 * 클라이언트 쪽: req 한 줄 전송 후 응답 한 줄 수신.
 * OK=resp 채움, UNAVAILABLE=연결 전 실패,
 * UNCERTAIN=연결된 제어 소켓의 IO 실패.
 */
int ctlsock_query(const char *path, const char *req, char *resp, size_t cap,
                  int timeout_ms);

#ifdef HAMN_TEST
void ctlsock_test_fail_nonblocking_once(int error);
#endif

#endif
