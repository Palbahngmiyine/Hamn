#include <string.h>
#include "cli.h"
#include "core/log.h"
#include "fwd/docker_observer.h"
#include "fwd/mount_inotify.h"
#include "fwd/udp_proxy.h"

/* Only same-binary process modes reach C. Public requests use the control ABI. */
int hamn_core_main(int argc, char **argv)
{
    cli_set_invocation_path(argc > 0 ? argv[0] : NULL);
    if (argc < 2) return 2;
    const char *cmd = argv[1];
    if (strcmp(cmd, "vmrun") == 0)
        return cmd_vmrun(argc - 1, argv + 1);
    if (strcmp(cmd, "qcow2-extract") == 0)
        return cmd_qcow2_extract(argc - 1, argv + 1);
    if (strcmp(cmd, "port-observer") == 0)
        return cmd_port_observer(argc - 1, argv + 1);
    if (strcmp(cmd, "mount-inotify-watch") == 0)
        return cmd_mount_inotify_watch(argc - 1, argv + 1);
    if (strcmp(cmd, "udp-forward") == 0)
        return cmd_udp_forward(argc - 1, argv + 1);
    logerr("unknown internal process mode");
    return 2;
}
