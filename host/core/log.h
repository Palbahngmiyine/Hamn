#ifndef HAMN_LOG_H
#define HAMN_LOG_H

#include <stdarg.h>

void logmsg(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
void logerr(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
/* Record the operation's failure reason without printing it. The frontend
 * reports log_last_error() exactly once (human line or machine JSON). */
void log_set_error(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
void die(const char *fmt, ...) __attribute__((format(printf, 1, 2), noreturn));
void log_set_machine_json(int enabled);
/* Start every line that logmsg/logerr/die print with the UTC time
 * (`2026-10-06T14:39:02Z `). For long-lived process modes, whose output is an
 * append-only log file. The recorded failure reason carries no time. */
void log_set_timestamps(int enabled);
void log_emit_machine_error(int exit_code);
const char *log_last_error(void);

#endif
