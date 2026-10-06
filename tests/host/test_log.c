/* Lines of a long-lived process mode start with the UTC time; the recorded
 * failure reason and every other process's output do not. */
#include <assert.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "core/log.h"

/* `YYYY-MM-DDTHH:MM:SSZ ` at the start of line; returns the rest, or NULL. */
static const char *after_timestamp(const char *line, time_t *stamp)
{
    static const char shape[] = "dddd-dd-ddTdd:dd:ddZ ";
    for (size_t i = 0; shape[i]; i++) {
        if (shape[i] == 'd' ? !(line[i] >= '0' && line[i] <= '9') :
            line[i] != shape[i])
            return NULL;
    }
    struct tm utc = {0};
    assert(sscanf(line, "%d-%d-%dT%d:%d:%dZ", &utc.tm_year, &utc.tm_mon,
                  &utc.tm_mday, &utc.tm_hour, &utc.tm_min, &utc.tm_sec) == 6);
    utc.tm_year -= 1900;
    utc.tm_mon -= 1;
    *stamp = timegm(&utc);
    return line + sizeof(shape) - 1;
}

int main(void)
{
    char path[] = "/tmp/hamn-log.XXXXXX";
    int fd = mkstemp(path);
    assert(fd >= 0);
    /* A daemon's stdout and stderr share one append-only log file. */
    assert(dup2(fd, STDOUT_FILENO) >= 0 && dup2(fd, STDERR_FILENO) >= 0);

    logmsg("plain %d", 1);
    logerr("plain failure %s", "x");
    time_t before = time(NULL);
    log_set_timestamps(1);
    logmsg("stamped %d", 2);
    logerr("stamped failure %s", "y");
    time_t after = time(NULL);
    assert(strcmp(log_last_error(), "stamped failure y") == 0);
    log_set_timestamps(0);
    logmsg("plain %d", 3);
    fflush(stdout);
    fflush(stderr);

    char text[1024] = {0};
    assert(lseek(fd, 0, SEEK_SET) == 0);
    ssize_t length = read(fd, text, sizeof(text) - 1);
    assert(length > 0);
    assert(close(fd) == 0 && unlink(path) == 0);

    const char *expected[] = { "plain 1", "hamn: plain failure x",
                               "stamped 2", "hamn: stamped failure y",
                               "plain 3" };
    char *line = text;
    for (size_t i = 0; i < sizeof(expected) / sizeof(expected[0]); i++) {
        char *end = strchr(line, '\n');
        assert(end);
        *end = '\0';
        time_t stamp = 0;
        const char *rest = after_timestamp(line, &stamp);
        if (i == 2 || i == 3) {
            assert(rest && strcmp(rest, expected[i]) == 0);
            assert(stamp >= before && stamp <= after);
        } else {
            assert(!rest && strcmp(line, expected[i]) == 0);
        }
        line = end + 1;
    }
    assert(*line == '\0');
    return 0;
}
