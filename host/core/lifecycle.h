#ifndef HAMN_LIFECYCLE_H
#define HAMN_LIFECYCLE_H

#include <stdint.h>

#include "core/profile.h"

#define VM_SPAWN_TOKEN_HEX_SIZE 33

enum vm_process_state {
    VM_PROCESS_STALE = 0,
    VM_PROCESS_VERIFIED,
    VM_PROCESS_UNVERIFIED,
};

enum vm_stop_result {
    VM_STOP_OK = 0,
    VM_STOP_FAILED = -1,
    VM_STOP_FORWARD_UNSAFE = -2,
};

struct vm_lifecycle_lock {
    int fd;
};

struct vm_spawned_process {
    int pid;
    uint64_t start_sec;
    uint64_t start_usec;
    unsigned char executable_uuid[16];
    int captured;
    int reaped;
};

enum vm_spawn_process_result {
    VM_SPAWN_PROCESS_ERROR = -1,
    VM_SPAWN_PROCESS_READY = 0,
    VM_SPAWN_PROCESS_EXITED = 1,
};

/* start/stop/delete와 fork된 supervisor 수명을 직렬화하는 inherited lock. */
int vm_lifecycle_lock_acquire(const char *profile_name,
                              struct vm_lifecycle_lock *lock);
void vm_lifecycle_lock_release(struct vm_lifecycle_lock *lock);

/*
 * spawn 전에 durable marker를 잠그고, vmrun이 identity를 기록할 때까지
 * inherited fd로 잠금을 넘긴다. 성공 handoff에서는 remove=0으로 parent
 * descriptor만 닫고, spawn 실패에서는 remove=1로 marker도 정리한다.
 */
int vm_spawn_guard_create(const struct profile *p, int *fd_out,
                          char token[VM_SPAWN_TOKEN_HEX_SIZE]);
int vm_spawn_guard_release(const struct profile *p, int fd, int remove);

/* 진행 중 spawn이 identity handoff를 끝내거나 사라질 때까지 bounded wait. */
int vm_process_wait_spawn_transition(const struct profile *p,
                                     int timeout_dsec);

/* ctl PID의 OS start token과 저장된 소유권을 확인해야 VERIFIED. */
enum vm_process_state vm_process_probe(const struct profile *p, int *pid);

/* direct child가 zombie인 동안 PID 재사용 전에 exact start/UUID를 캡처한다. */
enum vm_spawn_process_result vm_process_capture_spawned(
    int pid, struct vm_spawned_process *spawned);

/* vmrun 자식이 PID/start/executable identity를 durable하게 기록할 때까지 대기. */
enum vm_spawn_process_result vm_process_wait_spawned(
    const struct profile *p, struct vm_spawned_process *spawned,
    int timeout_dsec);

/* identity handshake 전 실패한 direct vmrun child만 exact UUID로 종료/reap. */
int vm_process_abort_spawned(struct vm_spawned_process *spawned,
                             int timeout_dsec);

/* 검증된 vmrun 프로세스이면 pid, 아니면 -1. */
int vm_running_pid(const struct profile *p);

/* 살아 있는 hamn vmrun이 없을 때만 stale host artifacts를 정리한다. */
int vm_cleanup_stale(const struct profile *p);

/*
 * graceful 정지 시퀀스:
 *   ssh poweroff → 25초 대기 → SIGTERM(vmrun이 requestStop→15초→강제)
 *   → 18초 대기 → SIGKILL.
 * 매 신호 전 ctl/PID/start token을 재검증하며, 불확실하면 상태를
 * 보존하고 실패한다. 종료 확인 후 ssh master 정리 + state.json 갱신.
 * was_running이 NULL이 아니면 실행 중이었는지 기록. VM_STOP_OK=성공.
 * VM_STOP_FORWARD_UNSAFE이면 추적 중인 host forward를 안전하게 정리하지
 * 못했으므로 profile의 ownership state를 삭제하면 안 된다.
 */
int vm_stop(const struct profile *p, int *was_running);

#endif
