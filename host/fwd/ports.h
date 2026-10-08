#ifndef HAMN_FWD_PORTS_H
#define HAMN_FWD_PORTS_H

#include <stddef.h>
#include <stdint.h>

#include "core/profile.h"

enum port_protocol {
    PORT_TCP,
    PORT_UDP,
};

struct port_spec {
    char host_ip[64];
    unsigned host_port;
    unsigned container_port;
    enum port_protocol protocol;
};

int port_number_parse(const char *text, unsigned *port);
int port_spec_parse(const char *text, struct port_spec *spec,
                    char *error, size_t error_cap);

/*
 * The host listeners of a profile change only through
 * port_forward_sync_docker_serialized() and port_forward_cleanup(), and every
 * change is made under this exclusive per-profile lock: the caller of the
 * synchronization takes it, and the cleanup takes it itself. The lock is a
 * file descriptor, or -1 when it cannot be taken; unlocking -1 does nothing.
 * The supervisor of a control command that a change runs inherits the
 * descriptor, so the lock stays held until that command has ended, even if
 * the caller dies first.
 */
int port_forward_operation_lock(const struct profile *p);
void port_forward_operation_unlock(int lock_fd);

/*
 * Stops every recorded forward of a stopping VM and removes its record. A
 * forward that cannot be stopped, or whose UDP relay cannot be identified,
 * keeps its record and makes the call return -1; the others are still
 * stopped. The call also returns -1, stopping nothing, when the state cannot
 * be locked or read.
 */
int port_forward_cleanup(const struct profile *p, const char *guest_ip);

/*
 * Reconcile the host listeners with a complete Docker inspect snapshot.
 * Caller must hold port_forward_operation_lock().  Docker is already the
 * authority for guest publication, so this function only changes host-side
 * listeners and their recovery records.
 *
 * A published TCP port is committed only by a forward request that the SSH
 * master answered with success. A pending TCP record, left by a request that
 * was reserved or sent without such an answer, is not evidence of a listener:
 * every call sends its request again, which a master that already holds the
 * forward answers with success. Until then the call returns -1 and
 * port_forward_failures() lists the port.
 */
int port_forward_sync_docker_serialized(const struct profile *p,
                                        const char *guest_ip,
                                        const struct port_spec specs[],
                                        int spec_count);

/*
 * The published ports that the last Docker synchronization could not forward,
 * as a new JSON array of {"hostIp","hostPort","protocol","reason"}. reason is
 * "hostPortInUse" when another process holds the host port and "forwardFailed"
 * otherwise, which includes a TCP port that cannot be bound while the SSH
 * master has not answered whether it holds that port itself. The array is
 * empty when there are none and when the profile has no valid record. The
 * caller deletes it; NULL means out of memory.
 *
 * The synchronization writes the record only when the set changes, and logs
 * each change once. It describes the port observer's last pass, so it is
 * meaningful only while the VM runs: the observer clears it when it starts
 * and a VM stop removes it.
 */
struct cJSON;
struct cJSON *port_forward_failures(const struct profile *p);
int port_forward_failures_clear(const struct profile *p);

#endif
