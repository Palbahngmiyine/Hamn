#ifndef HAMND_CRI_STATUS_H
#define HAMND_CRI_STATUS_H

#define HAMND_CONTAINERD_SOCKET "/run/containerd/containerd.sock"

/* Returns true only when containerd reports its CRI plugin as healthy. */
int cri_plugin_ready(void);

#ifdef HAMN_TEST
/* Test-only command injection; production always executes /usr/bin/ctr. */
int cri_plugin_ready_for_test(const char *ctr_path, unsigned timeout_ms);
#endif

#endif
