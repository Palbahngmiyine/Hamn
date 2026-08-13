#ifndef HAMN_CLOUDINIT_H
#define HAMN_CLOUDINIT_H

#include "core/profile.h"

/*
 * cloud-init NoCloud seed ISO(<profile>/seed.iso) 생성.
 * force=0이면 profile config보다 새 seed가 있을 때 재사용한다. force=1이면
 * 새 instance-id로 재생성해 per-instance 모듈(users 등)을 다시 실행시킨다.
 * 0=성공.
 */
int cloudinit_seed_ensure(const struct profile *p, int force);

#endif
