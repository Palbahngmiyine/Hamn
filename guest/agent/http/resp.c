#include "http/resp.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "version.h"

static const char *status_text(int s)
{
    switch (s) {
    case 200: return "OK";
    case 201: return "Created";
    case 204: return "No Content";
    case 304: return "Not Modified";
    case 400: return "Bad Request";
    case 404: return "Not Found";
    case 409: return "Conflict";
    case 500: return "Internal Server Error";
    case 503: return "Service Unavailable";
    default:  return "Unknown";
    }
}

static void send_response(struct conn *c, int status, const char *ctype,
                          const char *body, size_t blen)
{
    char hdr[512];
    int n = snprintf(hdr, sizeof(hdr),
                     "HTTP/1.1 %d %s\r\n"
                     "Server: hamnd/" HAMND_VERSION "\r\n"
                     "Content-Type: %s\r\n"
                     "Content-Length: %zu\r\n"
                     "\r\n",
                     status, status_text(status), ctype, blen);
    conn_write(c, hdr, (size_t)n);
    if (blen > 0)
        conn_write(c, body, blen);
}

void resp_text(struct conn *c, int status, const char *body)
{
    send_response(c, status, "text/plain; charset=utf-8", body,
                  strlen(body));
}

void resp_data(struct conn *c, int status, const char *content_type,
               const void *body, size_t len)
{
    send_response(c, status, content_type, body, len);
}

void resp_empty(struct conn *c, int status)
{
    send_response(c, status, "text/plain; charset=utf-8", "", 0);
}

void resp_json(struct conn *c, int status, cJSON *j)
{
    char *text = cJSON_PrintUnformatted(j);
    cJSON_Delete(j);
    if (!text) {
        resp_error(c, 500, "json serialization failed");
        return;
    }
    send_response(c, status, "application/json", text, strlen(text));
    free(text);
}

void resp_error(struct conn *c, int status, const char *fmt, ...)
{
    char msg[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof(msg), fmt, ap);
    va_end(ap);

    cJSON *j = cJSON_CreateObject();
    cJSON_AddStringToObject(j, "message", msg);
    char *text = cJSON_PrintUnformatted(j);
    cJSON_Delete(j);
    if (!text)
        return;
    send_response(c, status, "application/json", text, strlen(text));
    free(text);
}
