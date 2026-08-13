#include <fcntl.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *signal_marker;

static void mark_and_exit(int signal_number)
{
    int fd = open(signal_marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd >= 0)
        (void)close(fd);
    _exit(128 + signal_number);
}

int main(int argc, char **argv)
{
    if (argc != 3)
        return 2;
    signal_marker = argv[1];

    const int signals[] = { SIGINT, SIGTERM, SIGHUP };
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = mark_and_exit;
    sigemptyset(&action.sa_mask);
    for (size_t i = 0; i < sizeof(signals) / sizeof(signals[0]); i++) {
        if (sigaction(signals[i], &action, NULL) != 0)
            return 1;
    }

    int ready = open(argv[2], O_WRONLY);
    if (ready < 0 || write(ready, "ready\n", 6) != 6 || close(ready) != 0)
        return 1;
    for (;;)
        pause();
}
