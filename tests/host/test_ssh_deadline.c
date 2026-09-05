#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "sshmgr/ssh.h"
#include "util/proc.h"

static int completed(int rc, void *unused)
{
    (void)unused;
    assert(rc == PROC_RUN_TIMEOUT || rc == 255);
    puts("completion observed failure");
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv)
{
    assert(argc == 3);
    struct profile p = {0};
    snprintf(p.dir, sizeof(p.dir), "%s", argv[1]);
    const char *op = argv[2];
    if (strcmp(op, "alive") == 0)
        assert(ssh_master_alive(&p) == -1);
    else if (strcmp(op, "exit") == 0)
        ssh_master_exit(&p);
    else if (strcmp(op, "start") == 0)
        assert(ssh_master_start(&p, "127.0.0.1", 1) == -1);
    else if (strcmp(op, "forward") == 0) {
        int rc = ssh_forward_add_tcp_observed(&p, "127.0.0.1", "127.0.0.1",
            54321, "127.0.0.1", 54321, completed, NULL);
        assert(rc == PROC_RUN_TIMEOUT || rc == 255);
    }
    else if (strcmp(op, "cancel") == 0) {
        int rc = ssh_forward_cancel_tcp(&p, "127.0.0.1", "127.0.0.1",
            54321, "127.0.0.1", 54321);
        assert(rc == PROC_RUN_TIMEOUT || rc == 255);
    }
    else {
        assert(strcmp(op, "exec") == 0);
        const char *command[] = {"sudo", "systemctl", "poweroff", NULL};
        assert(ssh_exec_bounded(&p, "127.0.0.1", command, 200) == PROC_RUN_TIMEOUT);
    }
    return 0;
}
