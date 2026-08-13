#ifndef HAMND_RESP_H
#define HAMND_RESP_H

#include "cjson/cJSON.h"
#include "http/conn.h"

/* Content-Length 기반 단발 응답 헬퍼. 스트리밍(chunked)은 M3에서 추가. */

void resp_text(struct conn *c, int status, const char *body);
void resp_data(struct conn *c, int status, const char *content_type,
               const void *body, size_t len);
void resp_empty(struct conn *c, int status);
/* j를 직렬화해 전송하고 cJSON_Delete까지 수행한다 */
void resp_json(struct conn *c, int status, cJSON *j);
/* docker 에러 포맷 {"message":"..."} */
void resp_error(struct conn *c, int status, const char *fmt, ...)
    __attribute__((format(printf, 3, 4)));

#endif
