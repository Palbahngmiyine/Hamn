#ifndef HAMND_LOOP_H
#define HAMND_LOOP_H

#include <stdint.h>
#include <sys/epoll.h> /* 사용자 코드가 EPOLLIN 등 이벤트 상수를 쓴다 */

/*
 * epoll 기반 단일 스레드 이벤트 루프 (level-triggered).
 * blocking 작업(registry pull 등)은 M3에서 worker pool로 분리된다.
 */

struct loop;
typedef void (*loop_cb)(struct loop *l, int fd, uint32_t events, void *ud);

struct loop *loop_new(void);
int loop_add(struct loop *l, int fd, uint32_t events, loop_cb cb, void *ud);
int loop_mod(struct loop *l, int fd, uint32_t events, loop_cb cb, void *ud);
int loop_del(struct loop *l, int fd);
/* 종료 요청이 올 때까지 블록. 0=정상 종료 */
int loop_run(struct loop *l);
void loop_stop(struct loop *l);

#endif
