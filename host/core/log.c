#include "core/log.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cjson/cJSON.h"

static char last_error[8192];
static int machine_json;
static int machine_error_emitted;

void log_set_machine_json(int enabled)
{
    machine_json = enabled != 0;
    machine_error_emitted = 0;
    last_error[0] = '\0';
}

static const char *machine_error_code(int exit_code)
{
    if (exit_code == 2)
        return "invalidRequest";
    if (strstr(last_error, "approval") || strstr(last_error, "approved"))
        return "authenticationApprovalRequired";
    if (strstr(last_error, "version skew") ||
        strstr(last_error, "compatible kubectl"))
        return "kubectlIncompatible";
    if (strstr(last_error, "not running") ||
        strstr(last_error, "requires a running"))
        return "dependencyUnavailable";
    if (strstr(last_error, "read-only") || strstr(last_error, "refusing"))
        return "unsafeOperation";
    return "operationFailed";
}

void log_emit_machine_error(int exit_code)
{
    if (!machine_json || machine_error_emitted || exit_code == 0)
        return;
    machine_error_emitted = 1;
    const char *message = last_error[0] ? last_error :
        (exit_code == 2 ? "Invalid command arguments" : "Hamn operation failed");
    cJSON *root = cJSON_CreateObject();
    cJSON *error = cJSON_CreateObject();
    if (!root || !error ||
        !cJSON_AddNumberToObject(root, "schemaVersion", 1) ||
        !cJSON_AddStringToObject(error, "code",
                                 machine_error_code(exit_code)) ||
        !cJSON_AddStringToObject(error, "message", message) ||
        !cJSON_AddNumberToObject(error, "exitCode", exit_code) ||
        !cJSON_AddItemToObject(root, "error", error)) {
        cJSON_Delete(error);
        cJSON_Delete(root);
        return;
    }
    char *text = cJSON_PrintUnformatted(root);
    cJSON_Delete(root);
    if (text) {
        printf("%s\n", text);
        fflush(stdout);
        cJSON_free(text);
    }
}

void logmsg(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stdout, fmt, ap);
    va_end(ap);
    fputc('\n', stdout);
    fflush(stdout);
}

void logerr(const char *fmt, ...)
{
    va_list ap;
    va_list copy;
    fputs("hamn: ", stderr);
    va_start(ap, fmt);
    va_copy(copy, ap);
    vsnprintf(last_error, sizeof(last_error), fmt, copy);
    va_end(copy);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
}

void die(const char *fmt, ...)
{
    va_list ap;
    va_list copy;
    fputs("hamn: ", stderr);
    va_start(ap, fmt);
    va_copy(copy, ap);
    vsnprintf(last_error, sizeof(last_error), fmt, copy);
    va_end(copy);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
    log_emit_machine_error(1);
    exit(1);
}
