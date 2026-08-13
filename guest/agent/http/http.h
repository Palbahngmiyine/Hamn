#ifndef HAMND_HTTP_H
#define HAMND_HTTP_H

#include <stddef.h>

#include "picohttpparser/picohttpparser.h"

#define HTTP_MAX_HEADERS 40

/*
 * 파싱된 요청. method/path/query는 복사본이고, headers의 name/value와
 * body는 커넥션 수신 버퍼를 가리킨다 — 핸들러 실행 동안만 유효하다.
 */
struct http_req {
    char method[8];
    char path[2048];
    char query[1024];
    int minor_version;
    struct phr_header headers[HTTP_MAX_HEADERS];
    size_t nheaders;
    const char *body;
    size_t body_len;
    int keep_alive;
};

/* 대소문자 무시 헤더 조회. 없으면 NULL. *len에 값 길이. */
const char *http_req_header(const struct http_req *r, const char *name,
                            size_t *len);

/* query에서 키 값 추출 ("a=1&b=2"). 없으면 -1, 있으면 0 + out 채움 */
int http_req_query(const struct http_req *r, const char *key, char *out,
                   size_t cap);

#endif
