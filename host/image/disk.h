#ifndef HAMN_DISK_H
#define HAMN_DISK_H

#include "core/profile.h"

/*
 * <profile>/disk.img 준비: 없으면 캐시 qcow2에서 추출 후 설정 크기로
 * ftruncate(sparse 확장). 이미 있으면 설정이 더 클 때만 확장한다
 * (게스트의 cloud-init growpart가 부팅마다 파티션을 키운다).
 * 0=성공.
 */
int disk_prepare(const struct profile *p, const char *cache_img);

#endif
