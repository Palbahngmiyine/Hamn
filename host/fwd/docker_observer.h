#ifndef HAMN_FWD_DOCKER_OBSERVER_H
#define HAMN_FWD_DOCKER_OBSERVER_H

#include <stddef.h>

#include "core/profile.h"
#include "core/state.h"
#include "fwd/ports.h"

#define DOCKER_OBSERVER_MAX_PORTS 128

/*
 * Parse only NetworkSettings.Ports from one Docker inspect document.  The
 * caller provides the existing complete snapshot in specs/count; malformed
 * input leaves it untouched.
 */
int docker_observer_parse_inspect(const char *json,
                                  struct port_spec specs[], int *count,
                                  size_t capacity);

/* Parse a complete running-container list (Id and Ports). On error specs/count
 * are untouched. At most 2048 containers / MAX_PORTS published IPv4 TCP/UDP
 * mappings; unsupported IPv6/SCTP and exposed-only ports create no relay. */
int docker_observer_parse_list(const char *json, struct port_spec specs[],
                               int *count, size_t capacity);

/* One bounded list request through the private profile socket. */
int docker_observer_read_snapshot(const struct profile *profile,
                                  struct port_spec specs[], int *count,
                                  size_t capacity);

/* Returns 0 when synchronized, 1 when the lease was revoked, and -1 on error. */
int docker_observer_sync_once(const struct profile *profile,
                              const char *guest_ip, const char *lease);
int docker_observer_revoke(const struct profile *profile);
int docker_observer_start(const struct profile *profile,
                          const struct vm_state *state);
/* Internal observer loop; cycle_limit=0 runs until its lease is revoked. */
int docker_observer_watch(const struct profile *profile, const char *guest_ip,
                          const char *lease, unsigned cycle_limit);
int cmd_port_observer(int argc, char **argv);

#endif
