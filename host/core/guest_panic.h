#ifndef HAMN_GUEST_PANIC_H
#define HAMN_GUEST_PANIC_H

#include "cjson/cJSON.h"
#include "core/profile.h"

/*
 * The profile's record of the guest kernel panic that ended the VM's last
 * run, guest-panic.json: a private regular file written atomically by vmrun
 * just before it stops the VM (vmrun/console.h finds the panic on the guest
 * console). `vm start` removes it before it launches the next run, so a
 * record always describes the most recent run, which is no longer running
 * unless vmrun failed to stop it.
 *
 *     {"schemaVersion": 1, "detail": "<reason>", "observedAt": <seconds>}
 *
 * detail is the kernel's reason ("corrupted stack end detected inside
 * scheduler"), printable ASCII only, shorter than GUEST_PANIC_DETAIL_CAP.
 */

#define GUEST_PANIC_RECORD_NAME "guest-panic.json"
#define GUEST_PANIC_DETAIL_CAP 256

/*
 * vmrun: write the record at path (vmrun knows the profile only by its
 * files). detail must be printable ASCII shorter than GUEST_PANIC_DETAIL_CAP
 * ("" when the reason is unknown) and observed_at at least 0. Returns 0, or
 * -1 with errno (EINVAL for an argument outside that); the earlier file is
 * then unchanged.
 */
int guest_panic_record(const char *path, const char *detail,
                       long long observed_at);

/* vm start, before it launches vmrun: remove the record. Returns 0 when none
 * remains, or -1 with errno. */
int guest_panic_forget(const struct profile *profile);

/*
 * The record as `vm status` reports it: JSON null when there is none;
 * otherwise {"state": "panicked", "detail", "observedAt"}. A record that
 * cannot be validated (not this user's private regular file, not exactly the
 * writer's keys and types, a detail that is not printable ASCII) is
 * {"state": "unknown", "detail": <why>, "observedAt": null}. Returns NULL
 * only when memory runs out. Read-only.
 */
cJSON *guest_panic_snapshot(const struct profile *profile);

#endif
