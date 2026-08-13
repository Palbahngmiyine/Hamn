#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>

#include "api/router.h"
#include "http/server.h"
#include "loop/loop.h"
#include "version.h"

#define DEFAULT_SOCK "/run/hamnd.sock"
#define SOCK_GROUP "hamn"

int main(int argc, char **argv)
{
    const char *sock = DEFAULT_SOCK;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--sock") == 0 && i + 1 < argc) {
            sock = argv[++i];
        } else if (strcmp(argv[i], "--version") == 0) {
            printf("hamnd-agent %s\n", HAMND_VERSION);
            return 0;
        } else {
            fprintf(stderr, "usage: hamnd [--sock PATH]\n");
            return 2;
        }
    }

    if (signal(SIGPIPE, SIG_IGN) == SIG_ERR ||
        signal(SIGCHLD, SIG_DFL) == SIG_ERR) {
        perror("hamnd: configure signals");
        return 1;
    }
    struct loop *l = loop_new();
    if (!l || server_listen_unix(l, sock, SOCK_GROUP) != 0) {
        fprintf(stderr, "hamnd: cannot listen on %s\n", sock);
        return 1;
    }
    fprintf(stderr, "hamnd agent %s ready (docker + CRI status)\n",
            HAMND_VERSION);
    return loop_run(l);
}
