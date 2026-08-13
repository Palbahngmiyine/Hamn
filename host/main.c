#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cli.h"
#include "core/log.h"
#include "fwd/docker_observer.h"
#include "fwd/mount_inotify.h"
#include "fwd/udp_proxy.h"

static int usage(int rc)
{
    fprintf(rc ? stderr : stdout,
            "hamn %s - Docker-compatible containers on macOS\n"
            "\n"
            "usage: hamn <command> [args]\n"
            "\n"
            "commands:\n"
            "  start    create and start the VM\n"
            "           [--cpu N] [--memory GiB] [--disk GiB]\n"
            "           [--provision]\n"
            "  configure  save stopped-VM CPU, memory, and disk settings\n"
            "  stop     stop the VM\n"
            "  delete   soft-delete the VM; --data removes all profile data\n"
            "  status   show VM status\n"
            "  diagnostics  create a redacted support archive\n"
            "  list     list profiles\n"
            "  ssh      ssh into the VM             [-- command...]\n"
            "  template print the default profile YAML\n"
            "  env      print Docker SDK and Testcontainers exports\n"
            "  update   install a signed compatible release\n"
            "  uninstall remove managed install files and all Hamn data\n"
            "  kubernetes  manage this profile's K3s cluster\n"
            "  kubectl  run kubectl against this profile's K3s cluster\n"
            "  version  print version\n"
            "\n"
            "tip: install Docker CLI separately, run 'hamn start', then use\n"
            "     normal 'docker ...' commands through the Hamn context\n",
            HAMN_VERSION);
    return rc;
}

static int requests_json_output(int argc, char **argv)
{
    for (int i = 2; i + 1 < argc; i++) {
        if (strcmp(argv[i], "--output") == 0 &&
            strcmp(argv[i + 1], "json") == 0)
            return 1;
    }
    return 0;
}

int main(int argc, char **argv)
{
    cli_set_invocation_path(argc > 0 ? argv[0] : NULL);
    if (argc < 2)
        return usage(1);

    const char *cmd = argv[1];
    int sub_argc = argc - 1;
    char **sub_argv = argv + 1;
    int machine_json = requests_json_output(argc, argv) &&
        (strcmp(cmd, "status") == 0 || strcmp(cmd, "configure") == 0 ||
         strcmp(cmd, "diagnostics") == 0 ||
         strcmp(cmd, "kubernetes") == 0);
    log_set_machine_json(machine_json);

    if (strcmp(cmd, "version") == 0 || strcmp(cmd, "--version") == 0) {
        printf("hamn %s\n", HAMN_VERSION);
        return 0;
    }
    if (strcmp(cmd, "help") == 0 || strcmp(cmd, "--help") == 0)
        return usage(0);
    int rc;
    if (strcmp(cmd, "start") == 0)
        rc = cmd_start(sub_argc, sub_argv);
    else if (strcmp(cmd, "configure") == 0)
        rc = cmd_configure(sub_argc, sub_argv);
    else if (strcmp(cmd, "stop") == 0)
        rc = cmd_stop(sub_argc, sub_argv);
    else if (strcmp(cmd, "delete") == 0)
        rc = cmd_delete(sub_argc, sub_argv);
    else if (strcmp(cmd, "status") == 0)
        rc = cmd_status(sub_argc, sub_argv);
    else if (strcmp(cmd, "diagnostics") == 0)
        rc = cmd_diagnostics(sub_argc, sub_argv);
    else if (strcmp(cmd, "list") == 0)
        rc = cmd_list(sub_argc, sub_argv);
    else if (strcmp(cmd, "ssh") == 0)
        rc = cmd_ssh(sub_argc, sub_argv);
    else if (strcmp(cmd, "template") == 0)
        rc = cmd_template(sub_argc, sub_argv);
    else if (strcmp(cmd, "env") == 0)
        rc = cmd_env(sub_argc, sub_argv);
    else if (strcmp(cmd, "update") == 0)
        rc = cmd_update(sub_argc, sub_argv);
    else if (strcmp(cmd, "uninstall") == 0)
        rc = cmd_uninstall(sub_argc, sub_argv);
    else if (strcmp(cmd, "kubernetes") == 0)
        rc = cmd_kubernetes(sub_argc, sub_argv);
    else if (strcmp(cmd, "kubectl") == 0)
        rc = cmd_kubectl(sub_argc, sub_argv);
    else if (strcmp(cmd, "vmrun") == 0)
        rc = cmd_vmrun(sub_argc, sub_argv);
    else if (strcmp(cmd, "qcow2-extract") == 0)
        rc = cmd_qcow2_extract(sub_argc, sub_argv);
    else if (strcmp(cmd, "port-observer") == 0)
        rc = cmd_port_observer(sub_argc, sub_argv);
    else if (strcmp(cmd, "mount-inotify-watch") == 0)
        rc = cmd_mount_inotify_watch(sub_argc, sub_argv);
    else if (strcmp(cmd, "udp-forward") == 0)
        rc = cmd_udp_forward(sub_argc, sub_argv);
    else {
        logerr("unknown command '%s'", cmd);
        rc = usage(1);
    }
    log_emit_machine_error(rc);
    return rc;
}
