#ifndef HAMN_ADMISSION_H
#define HAMN_ADMISSION_H

/*
 * Admission: before vm start launches a VM, decide from the host's current
 * usage whether it may. A refusal starts nothing. Admission never stops,
 * pauses or deletes a VM.
 *
 * admission_decide is the decision, a pure function of a snapshot, the
 * request and the limits. admission_measure reads the snapshot.
 * admission_admit runs both under the admission lock.
 *
 * Concurrency: a start holds the admission lock from its measurement until
 * the VM it launches is visible to vm_process_probe (or until it fails), so
 * the next start's measurement counts that VM and two starts never pass on
 * the same headroom. No reservation is stored: a VM that stops or whose
 * vmrun dies stops counting because the probe no longer sees it.
 *
 * Lock order: a profile's lifecycle lock, then its mutation lock, then the
 * admission lock. Nothing may take a profile lock while it holds the
 * admission lock.
 */

#include <stddef.h>
#include <stdint.h>

#include "core/profile.h"

/* kern.memorystatus_vm_pressure_level. XNU reports its internal level as
 * the dispatch memory pressure value (DISPATCH_MEMORYPRESSURE_* in
 * <dispatch/source.h>); its warning and urgent levels are both WARN. */
enum admission_pressure {
    ADMISSION_PRESSURE_NORMAL = 1,
    ADMISSION_PRESSURE_WARN = 2,
    ADMISSION_PRESSURE_CRITICAL = 4,
};

/* The source the host draws power from (IOPSGetProvidingPowerSourceType). */
enum admission_power {
    ADMISSION_POWER_AC,
    ADMISSION_POWER_BATTERY,
    ADMISSION_POWER_UPS,
};

/* One measurement, as a bit of admission_snapshot.unread. */
enum admission_item {
    ADMISSION_ITEM_PRESSURE = 1u << 0,
    ADMISSION_ITEM_TOTAL_MEMORY = 1u << 1,
    ADMISSION_ITEM_RUNNING_MEMORY = 1u << 2,
    ADMISSION_ITEM_LOAD = 1u << 3,
    ADMISSION_ITEM_CPUS = 1u << 4,
    ADMISSION_ITEM_POWER = 1u << 5,   /* the providing source */
    ADMISSION_ITEM_BATTERY = 1u << 6, /* the internal battery's charge */
};

/* A profile whose VM runs, or may run: vm_process_probe could not verify
 * its vmrun, which a start does in the moment after its VM became visible. */
struct admission_vm {
    char name[PROFILE_NAME_CAP];
    unsigned memory_mib; /* configured memoryMiB; 0 when unreadable */
    int verified;        /* 1 when the probe verified the vmrun */
};

#define ADMISSION_FAILURE_CAP (PROFILE_FAILURE_CAP + 64)

/* The host at one moment. A field whose item is in unread is meaningless. */
struct admission_snapshot {
    unsigned unread;                  /* admission_item bits */
    enum admission_pressure pressure;
    uint64_t total_memory_mib;        /* hw.memsize */
    uint64_t running_memory_mib;      /* the sum of vms[].memory_mib */
    double load_1m;                   /* getloadavg, at least 0 */
    unsigned logical_cpus;            /* hw.logicalcpu, at least 1 */
    enum admission_power power;
    int battery_percent;              /* internal battery, 0..100; -1: none */
    struct admission_vm *vms;         /* owned, vm_count entries */
    size_t vm_count;
    /* Why the running memory is unread: names the profile or directory. */
    char running_failure[ADMISSION_FAILURE_CAP];
};

struct admission_request {
    unsigned cpus;
    unsigned memory_mib;
};

/* Every limit has a value that refuses nothing it governs, except the
 * memory pressure limit, which is WARN or CRITICAL. */
struct admission_limits {
    uint64_t memory_reserve_mib;       /* host memory no VM may use */
    enum admission_pressure refused_pressure; /* refuse at or above */
    double max_load_per_cpu;           /* refuse above; INFINITY: no limit */
    unsigned min_battery_percent;      /* refuse on battery below; 0..100 */
};

/* Why a start is refused, as bits of admission_decide's result. */
enum admission_reason {
    ADMISSION_REFUSED_UNREAD = 1u << 0,
    ADMISSION_REFUSED_PRESSURE = 1u << 1,
    ADMISSION_REFUSED_MEMORY = 1u << 2,
    ADMISSION_REFUSED_LOAD = 1u << 3,
    ADMISSION_REFUSED_BATTERY = 1u << 4,
};

/* 0 to admit, or every reason that refuses. Reads only its arguments.
 *   UNREAD    an item an enabled limit uses is unread. PRESSURE,
 *             TOTAL_MEMORY and RUNNING_MEMORY are always used; LOAD and
 *             CPUS when max_load_per_cpu is finite; POWER when
 *             min_battery_percent > 0, and BATTERY too when POWER reads
 *             BATTERY. Any other unread item refuses nothing. A check that
 *             needs only read items still adds its own reason;
 *   PRESSURE  pressure >= refused_pressure;
 *   MEMORY    running_memory_mib + memory_mib > total_memory_mib -
 *             memory_reserve_mib (0 when the reserve is not smaller);
 *   LOAD      load_1m / logical_cpus > max_load_per_cpu;
 *   BATTERY   power is BATTERY and battery_percent < min_battery_percent.
 * On UPS power the battery limit does not apply. */
unsigned admission_decide(const struct admission_snapshot *snapshot,
                          const struct admission_request *request,
                          const struct admission_limits *limits);

/* The limits from the environment; an unset or empty variable keeps the
 * default, which refuses only at critical memory pressure and when the VMs
 * would exceed the host's memory:
 *   HAMN_ADMISSION_MEMORY_RESERVE_MIB   decimal MiB, at most 4294967295; 0
 *   HAMN_ADMISSION_MEMORY_PRESSURE      warn or critical; critical
 *   HAMN_ADMISSION_MAX_LOAD_PER_CPU     a positive decimal such as 1.5;
 *                                       no limit
 *   HAMN_ADMISSION_MIN_BATTERY_PERCENT  decimal 0 to 100; 0
 * Returns 0, or -1 after logerr names the variable and the rule. */
int admission_limits_read(struct admission_limits *limits);

/* Reads the snapshot: the sysctls kern.memorystatus_vm_pressure_level,
 * hw.memsize and hw.logicalcpu, getloadavg, the IOKit power sources, and
 * vm_process_probe of every profile under ~/.hamn except starting, adding
 * the memoryMiB of each one whose VM is verified or cannot be verified. A
 * value that cannot be read or is out of range marks its item unread; so
 * does a running profile whose config.yaml cannot be read, and battery
 * power without an internal battery marks BATTERY. Writes nothing
 * of its own; the probe can record a verified vmrun's identity in that
 * profile, as vm list does. Does not lock profiles. Release with
 * admission_snapshot_release. */
void admission_measure(const char *starting,
                       struct admission_snapshot *snapshot);
void admission_snapshot_release(struct admission_snapshot *snapshot);

/* The exclusive lock on ~/.hamn/.locks/.admission.lock. Its name cannot be
 * a profile's lifecycle lock (<profile>.lock), since profile names have no
 * dot. The descriptor is close-on-exec, so a launched vmrun never holds it. */
struct admission_lock {
    int fd; /* -1 when not held */
};

/* Takes the lock, waiting for it. While another holds it, says once that it
 * waits (logmsg) and polls every 100 ms, so a cancellation (proc_cancelled)
 * ends the wait: -1 with errno ECANCELED. -1 with errno for a lock file
 * that cannot be opened (vm_lock_file_open). */
int admission_lock_acquire(struct admission_lock *lock);
/* Releases a held lock; does nothing for one that is not held. */
void admission_lock_release(struct admission_lock *lock);

#define ADMISSION_REFUSED 1

/* Admission of a VM of profile with request, which vm start calls while it
 * holds the profile's lifecycle and mutation locks.
 *   0  admitted: *lock is held, and the caller releases it once its VM is
 *      visible to vm_process_probe or its start failed;
 *   ADMISSION_REFUSED  *refusal owns the refusal as UTF-8 JSON (release
 *      it with cJSON_free), log_last_error() holds the same as a
 *      sentence, and *lock is not held;
 *   -1 an invalid limit, a lock that cannot be taken, or a cancelled wait,
 *      with the reason in log_last_error(); *lock is not held.
 * The JSON is {"admission":"refused","profile","request":{"cpus",
 * "memoryMiB"},"reasons","unread","measured":{"memoryPressure",
 * "totalMemoryMiB","runningMemoryMiB","loadAverage1m","logicalCpus",
 * "powerSource","batteryPercent"},"limits":{"memoryReserveMiB",
 * "memoryPressure","maxLoadPerCpu","minBatteryPercent"},"runningProfiles":
 * [{"name","memoryMiB","verified"}]}. reasons holds "measurementFailed",
 * "memoryPressure", "memory", "load" and "battery" in that order; unread
 * holds every measured key that could not be read, whether or not a limit
 * uses it, and those values are null.
 * A pressure is "normal", "warn" or "critical", a power source "ac",
 * "battery" or "ups". batteryPercent is null without a battery, a memoryMiB
 * null when unreadable, and maxLoadPerCpu null without a load limit. */
int admission_admit(const char *profile, const struct admission_request *request,
                    struct admission_lock *lock, char **refusal);

#endif
