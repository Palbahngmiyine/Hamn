#ifndef HAMN_CLI_H
#define HAMN_CLI_H

#ifndef HAMN_VERSION
#define HAMN_VERSION "0.1.0-dev"
#endif

int cmd_start(int argc, char **argv);
int cmd_configure(int argc, char **argv);
int cmd_stop(int argc, char **argv);
int cmd_delete(int argc, char **argv);
int cmd_status(int argc, char **argv);
int cmd_diagnostics(int argc, char **argv);
int cmd_list(int argc, char **argv);
int cmd_ssh(int argc, char **argv);
int cmd_kubernetes(int argc, char **argv);
int cmd_kubectl(int argc, char **argv);
int cmd_template(int argc, char **argv);
int cmd_env(int argc, char **argv);
int cmd_uninstall(int argc, char **argv);
int cmd_update(int argc, char **argv);

/* The original argv[0] lets uninstall remove only the link that invoked us. */
void cli_set_invocation_path(const char *path);
const char *cli_invocation_path(void);

int cmd_vmrun(int argc, char **argv);
int cmd_qcow2_extract(int argc, char **argv);
int cmd_udp_forward(int argc, char **argv);

#endif
