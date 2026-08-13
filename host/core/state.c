#include "core/state.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cjson/cJSON.h"
#include "core/log.h"
#include "util/fs.h"

int state_load(const struct profile *p, struct vm_state *st)
{
    memset(st, 0, sizeof(*st));
    snprintf(st->state, sizeof(st->state), "stopped");

    char path[1024];
    profile_path(p, "state.json", path, sizeof(path));
    FILE *f = fopen(path, "r");
    if (!f)
        return 0;

    char buf[4096];
    size_t n = fread(buf, 1, sizeof(buf) - 1, f);
    if (ferror(f)) {
        logerr("cannot read state file %s: %s", path, strerror(errno));
        fclose(f);
        return -1;
    }
    if (n == sizeof(buf) - 1 && !feof(f)) {
        logerr("state file %s is too large", path);
        fclose(f);
        return -1;
    }
    fclose(f);
    buf[n] = '\0';

    cJSON *j = cJSON_Parse(buf);
    if (!j) {
        logerr("state file %s is not valid JSON", path);
        return -1;
    }

    const cJSON *v;
    if ((v = cJSON_GetObjectItem(j, "state")) && cJSON_IsString(v))
        snprintf(st->state, sizeof(st->state), "%s", v->valuestring);
    if ((v = cJSON_GetObjectItem(j, "ip")) && cJSON_IsString(v))
        snprintf(st->ip, sizeof(st->ip), "%s", v->valuestring);
    if ((v = cJSON_GetObjectItem(j, "started_at")) && cJSON_IsNumber(v))
        st->started_at = (long long)v->valuedouble;
    if ((v = cJSON_GetObjectItem(j, "prev_docker_context")) &&
        cJSON_IsString(v))
        snprintf(st->prev_docker_context, sizeof(st->prev_docker_context),
                 "%s", v->valuestring);
    if ((v = cJSON_GetObjectItem(j, "prev_kube_context")) &&
        cJSON_IsString(v))
        snprintf(st->prev_kube_context, sizeof(st->prev_kube_context),
                 "%s", v->valuestring);
    cJSON_Delete(j);
    return 0;
}

int state_save(const struct profile *p, const struct vm_state *st)
{
    cJSON *j = cJSON_CreateObject();
    if (!j)
        return -1;
    if (!cJSON_AddStringToObject(j, "state", st->state) ||
        !cJSON_AddStringToObject(j, "ip", st->ip) ||
        !cJSON_AddNumberToObject(j, "started_at", (double)st->started_at)) {
        cJSON_Delete(j);
        return -1;
    }
    if (st->prev_docker_context[0] &&
        !cJSON_AddStringToObject(j, "prev_docker_context",
                                 st->prev_docker_context)) {
        cJSON_Delete(j);
        return -1;
    }
    if (st->prev_kube_context[0] &&
        !cJSON_AddStringToObject(j, "prev_kube_context",
                                 st->prev_kube_context)) {
        cJSON_Delete(j);
        return -1;
    }
    char *text = cJSON_Print(j);
    cJSON_Delete(j);
    if (!text)
        return -1;

    char path[1024];
    profile_path(p, "state.json", path, sizeof(path));
    size_t len = strlen(text);
    int rc = fs_write_file_atomic(path, text, len, 0600);
    free(text);
    return rc;
}
