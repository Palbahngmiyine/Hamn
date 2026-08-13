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

/*
 * Identifies one host wrapper invocation across PID reuse.  A late completion
 * may only mutate forwarding records that still belong to this generation.
 */
struct port_forward_generation {
    int owner_pid;
    uint64_t owner_start_sec;
    uint64_t owner_start_usec;
};

int port_number_parse(const char *text, unsigned *port);
int port_spec_parse(const char *text, struct port_spec *spec,
                    char *error, size_t error_cap);
int port_spec_guest_text(const struct port_spec *spec, const char *guest_ip,
                         char *text, size_t cap);
int port_forward_operation_lock(const struct profile *p);
void port_forward_operation_unlock(int lock_fd);
int port_forward_generation_current(struct port_forward_generation *generation);
int port_forward_add(const struct profile *p, const char *guest_ip,
                     const struct port_spec *spec);
/* Caller must hold port_forward_operation_lock(). */
int port_forward_add_serialized(const struct profile *p, const char *guest_ip,
                                const struct port_spec *spec);
int port_forward_submit_many(const struct profile *p,
                             const struct port_spec specs[], int spec_count,
                             int serialized,
                             const struct port_forward_generation *generation);
int port_forward_commit(const struct profile *p,
                        const struct port_spec *spec);
int port_forward_commit_owned(
    const struct profile *p, const struct port_spec *spec,
    const struct port_forward_generation *generation);
int port_forward_remove(const struct profile *p, const char *guest_ip,
                        const struct port_spec *spec);
int port_forward_remove_owned(
    const struct profile *p, const char *guest_ip,
    const struct port_spec *spec,
    const struct port_forward_generation *generation);
/* Caller must hold port_forward_operation_lock(). */
int port_forward_remove_serialized(const struct profile *p,
                                   const char *guest_ip,
                                   const struct port_spec *spec);
/* Caller must hold port_forward_operation_lock(). */
int port_forward_remove_owned_serialized(
    const struct profile *p, const char *guest_ip,
    const struct port_spec *spec,
    const struct port_forward_generation *generation);
int port_forward_cleanup(const struct profile *p, const char *guest_ip);
int port_forward_reconcile(const struct profile *p, const char *guest_ip,
                           const char *published_ports);
int port_forward_reconcile_serialized(const struct profile *p,
                                      const char *guest_ip,
                                      const char *published_ports);
/* Caller must hold port_forward_operation_lock(). */
int port_forward_reconcile_rm_serialized(const struct profile *p,
                                         const char *guest_ip,
                                         const char *previous_published_ports,
                                         const char *published_ports);
/*
 * Reconcile the host listeners with a complete Docker inspect snapshot.
 * Caller must hold port_forward_operation_lock().  Docker is already the
 * authority for guest publication, so this function only changes host-side
 * listeners and their recovery records.
 */
int port_forward_sync_docker_serialized(const struct profile *p,
                                        const char *guest_ip,
                                        const struct port_spec specs[],
                                        int spec_count);

#endif
