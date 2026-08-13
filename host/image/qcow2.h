#ifndef HAMN_QCOW2_H
#define HAMN_QCOW2_H

/*
 * 읽기 전용 qcow2 → raw 추출기 (스파이크 S1).
 *
 * 지원 범위는 Ubuntu cloud image가 사용하는 부분집합으로 한정한다:
 * qcow2 v2/v3, backing file 없음, 암호화 없음, 압축은 deflate만.
 * 그 외 기능(extended L2, external data file, zstd 등)은 명시적으로 거부한다.
 *
 * 출력 raw 파일은 가상 크기로 ftruncate되며, 미할당/제로 클러스터는
 * 기록하지 않아 sparse 파일이 된다.
 *
 * 성공 시 0, 실패 시 -1을 반환하고 *err에 malloc된 메시지를 채운다.
 */
int qcow2_extract(const char *in_path, const char *out_path, char **err);

#endif
