#ifndef HAMN_LOG_H
#define HAMN_LOG_H

#include <stdarg.h>

void logmsg(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
void logerr(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
void die(const char *fmt, ...) __attribute__((format(printf, 1, 2), noreturn));
void log_set_machine_json(int enabled);
void log_emit_machine_error(int exit_code);

#endif
