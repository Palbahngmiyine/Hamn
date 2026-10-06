#ifndef HAMN_FS_H
#define HAMN_FS_H

#include <sys/types.h>

/* mkdir -p. 이미 존재하면 성공. 실패 시 -1, errno 유지. */
int fs_mkdirs(const char *path, mode_t mode);

/* unlink. 파일이 이미 없으면 성공. 실패 시 -1, errno 유지. */
int fs_unlink_if_exists(const char *path);

/* tmp 파일에 쓴 뒤 fsync+rename+parent fsync. 실패 시 -1, errno 유지. */
int fs_write_file_atomic(const char *path, const char *data, size_t len,
                         mode_t mode);

/* Space available to this user on the volume that holds path, in MiB.
 * Returns -1 with errno set when the volume cannot be queried. */
int fs_free_mib(const char *path, unsigned long long *mib);

#ifdef HAMN_TEST
void fs_test_fail_parent_fsync_once(int error);
#endif
#endif
