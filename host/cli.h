#ifndef HAMN_CLI_H
#define HAMN_CLI_H

/* HAMN_VERSION is defined only for the Makefile's VERSIONED_OBJS; anywhere
 * else it is an undeclared identifier, so a new use cannot silently build
 * a wrong version. */

/* The original argv[0] lets uninstall remove only the link that invoked us. */
void cli_set_invocation_path(const char *path);
const char *cli_invocation_path(void);

int cmd_vmrun(int argc, char **argv);
int cmd_qcow2_extract(int argc, char **argv);
int cmd_udp_forward(int argc, char **argv);

#endif
