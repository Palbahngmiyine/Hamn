#ifndef HAMND_SERVER_H
#define HAMND_SERVER_H

#include "loop/loop.h"

/*
 * unix socket 리스너. root:<group> 0660 권한 설정까지 성공해야 한다.
 * group이 없거나 권한 설정이 실패하면 소켓을 닫고 오류를 반환한다.
 */
int server_listen_unix(struct loop *l, const char *path, const char *group);

#endif
