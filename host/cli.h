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
/* "hamn workspace-reclaim <record>": what vmrun starts when it stopped the
 * VM because the workspace in the record is gone (core/workspace.h). Purges
 * the profile of that record, waiting for its lifecycle lock, when the record
 * is still the profile's and its VM is known to be stopped; leaves it alone
 * otherwise. A detached process with no frontend: it needs none of the
 * worker's cancellation or protocol set-up, and its lines go to the log it
 * was started with. Returns 0 also when it left the profile alone or found
 * none, 2 for arguments not a record, 1 when the purge failed. */
int cmd_workspace_reclaim(int argc, char **argv);

#endif
