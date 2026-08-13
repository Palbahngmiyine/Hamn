#ifndef HAMN_STATE_H
#define HAMN_STATE_H

#include "core/profile.h"

/*
 * state.json — 마지막으로 알려진 VM 상태 (advisory).
 * 실제 동작 여부는 항상 vmrun.pid/ctl 소켓으로 교차 확인한다.
 */

struct vm_state {
    char state[16];   /* "starting" | "running" | "stopped" */
    char ip[64];
    long long started_at;
    char prev_docker_context[128]; /* M2: stop 시 복원할 docker context */
    char prev_kube_context[128];   /* K3s start가 바꾼 current-context */
};

/* 파일 없으면 state="stopped"로 채우고 0 반환 */
int state_load(const struct profile *p, struct vm_state *st);
int state_save(const struct profile *p, const struct vm_state *st);

#endif
