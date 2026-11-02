#include "core/admission.h"

#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/ps/IOPSKeys.h>
#include <IOKit/ps/IOPowerSources.h>
#include <dirent.h>
#include <errno.h>
#include <math.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/sysctl.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "util/proc.h"

#define MIB (1024ULL * 1024ULL)
#define LOCK_POLL_US (100 * 1000)

static const struct {
    unsigned item;
    const char *key;   /* under "measured" */
    const char *words; /* in the message */
} items[] = {
    { ADMISSION_ITEM_PRESSURE, "memoryPressure", "memory pressure" },
    { ADMISSION_ITEM_TOTAL_MEMORY, "totalMemoryMiB", "total memory" },
    { ADMISSION_ITEM_RUNNING_MEMORY, "runningMemoryMiB", "running VMs' memory" },
    { ADMISSION_ITEM_LOAD, "loadAverage1m", "load average" },
    { ADMISSION_ITEM_CPUS, "logicalCpus", "CPU count" },
    { ADMISSION_ITEM_POWER, "powerSource", "power source" },
    { ADMISSION_ITEM_BATTERY, "batteryPercent", "battery charge" },
};

static const struct {
    unsigned reason;
    const char *code;
} reasons[] = {
    { ADMISSION_REFUSED_UNREAD, "measurementFailed" },
    { ADMISSION_REFUSED_PRESSURE, "memoryPressure" },
    { ADMISSION_REFUSED_MEMORY, "memory" },
    { ADMISSION_REFUSED_LOAD, "load" },
    { ADMISSION_REFUSED_BATTERY, "battery" },
};

#define COUNT(array) (sizeof(array) / sizeof((array)[0]))

/* The items the enabled limits use, as admission_decide describes. */
static unsigned needed_items(const struct admission_snapshot *snapshot,
                             const struct admission_limits *limits)
{
    unsigned needed = ADMISSION_ITEM_PRESSURE | ADMISSION_ITEM_TOTAL_MEMORY |
                      ADMISSION_ITEM_RUNNING_MEMORY;
    if (isfinite(limits->max_load_per_cpu))
        needed |= ADMISSION_ITEM_LOAD | ADMISSION_ITEM_CPUS;
    if (limits->min_battery_percent > 0) {
        needed |= ADMISSION_ITEM_POWER;
        if (!(snapshot->unread & ADMISSION_ITEM_POWER) &&
            snapshot->power == ADMISSION_POWER_BATTERY)
            needed |= ADMISSION_ITEM_BATTERY;
    }
    return needed;
}

unsigned admission_decide(const struct admission_snapshot *snapshot,
                          const struct admission_request *request,
                          const struct admission_limits *limits)
{
    unsigned unread = snapshot->unread, refused = 0;
    if (unread & needed_items(snapshot, limits))
        refused |= ADMISSION_REFUSED_UNREAD;
    if (!(unread & ADMISSION_ITEM_PRESSURE) &&
        snapshot->pressure >= limits->refused_pressure)
        refused |= ADMISSION_REFUSED_PRESSURE;
    if (!(unread & (ADMISSION_ITEM_TOTAL_MEMORY |
                    ADMISSION_ITEM_RUNNING_MEMORY))) {
        uint64_t available =
            snapshot->total_memory_mib > limits->memory_reserve_mib ?
            snapshot->total_memory_mib - limits->memory_reserve_mib : 0;
        if (snapshot->running_memory_mib + request->memory_mib > available)
            refused |= ADMISSION_REFUSED_MEMORY;
    }
    if (!(unread & (ADMISSION_ITEM_LOAD | ADMISSION_ITEM_CPUS)) &&
        snapshot->load_1m / snapshot->logical_cpus > limits->max_load_per_cpu)
        refused |= ADMISSION_REFUSED_LOAD;
    if (!(unread & (ADMISSION_ITEM_POWER | ADMISSION_ITEM_BATTERY)) &&
        snapshot->power == ADMISSION_POWER_BATTERY &&
        snapshot->battery_percent < (int)limits->min_battery_percent)
        refused |= ADMISSION_REFUSED_BATTERY;
    return refused;
}

/* The variable's value, or NULL when it is unset or empty. */
static const char *setting(const char *name)
{
    const char *value = getenv(name);
    return value && value[0] ? value : NULL;
}

static int decimal(const char *text, uint64_t max, uint64_t *value)
{
    if (text[strspn(text, "0123456789")] != '\0')
        return -1;
    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(text, &end, 10);
    if (errno || *end || parsed > max)
        return -1;
    *value = parsed;
    return 0;
}

/* A positive decimal fraction such as 1.5: digits with at most one point. */
static int positive_fraction(const char *text, double *value)
{
    const char *point = strchr(text, '.');
    if (text[strspn(text, "0123456789.")] != '\0' ||
        (point && strchr(point + 1, '.')) || strcmp(text, ".") == 0)
        return -1;
    char *end = NULL;
    double parsed = strtod(text, &end);
    if (*end || !isfinite(parsed) || parsed <= 0)
        return -1;
    *value = parsed;
    return 0;
}

int admission_limits_read(struct admission_limits *limits)
{
    *limits = (struct admission_limits){
        .memory_reserve_mib = 0,
        .refused_pressure = ADMISSION_PRESSURE_CRITICAL,
        .max_load_per_cpu = INFINITY,
        .min_battery_percent = 0,
    };
    const char *text;
    uint64_t value;
    if ((text = setting("HAMN_ADMISSION_MEMORY_RESERVE_MIB"))) {
        if (decimal(text, UINT32_MAX, &value) != 0) {
            logerr("HAMN_ADMISSION_MEMORY_RESERVE_MIB must be a decimal number "
                   "of MiB from 0 to 4294967295");
            return -1;
        }
        limits->memory_reserve_mib = value;
    }
    if ((text = setting("HAMN_ADMISSION_MEMORY_PRESSURE"))) {
        if (strcmp(text, "warn") == 0) {
            limits->refused_pressure = ADMISSION_PRESSURE_WARN;
        } else if (strcmp(text, "critical") != 0) {
            logerr("HAMN_ADMISSION_MEMORY_PRESSURE must be warn or critical");
            return -1;
        }
    }
    if ((text = setting("HAMN_ADMISSION_MAX_LOAD_PER_CPU")) &&
        positive_fraction(text, &limits->max_load_per_cpu) != 0) {
        logerr("HAMN_ADMISSION_MAX_LOAD_PER_CPU must be a positive decimal "
               "number such as 1.5");
        return -1;
    }
    if ((text = setting("HAMN_ADMISSION_MIN_BATTERY_PERCENT"))) {
        if (decimal(text, 100, &value) != 0) {
            logerr("HAMN_ADMISSION_MIN_BATTERY_PERCENT must be a decimal "
                   "number from 0 to 100");
            return -1;
        }
        limits->min_battery_percent = (unsigned)value;
    }
    return 0;
}

static int sysctl_value(const char *name, void *value, size_t size)
{
    size_t length = size;
    return sysctlbyname(name, value, &length, NULL, 0) == 0 &&
           length == size ? 0 : -1;
}

static int dictionary_int(CFDictionaryRef dictionary, CFStringRef key,
                          int *value)
{
    CFTypeRef number = CFDictionaryGetValue(dictionary, key);
    return number && CFGetTypeID(number) == CFNumberGetTypeID() &&
           CFNumberGetValue((CFNumberRef)number, kCFNumberIntType, value) ?
           0 : -1;
}

/* Reads the internal battery's charge into battery_percent, which stays -1
 * when the list has no internal battery. -1 when the list or that battery's
 * charge cannot be read. */
static int measure_battery(CFTypeRef info, struct admission_snapshot *snapshot)
{
    CFArrayRef sources = IOPSCopyPowerSourcesList(info);
    if (!sources)
        return -1;
    int rc = 0;
    for (CFIndex index = 0; index < CFArrayGetCount(sources); index++) {
        CFDictionaryRef description = IOPSGetPowerSourceDescription(
            info, CFArrayGetValueAtIndex(sources, index));
        if (!description)
            continue;
        CFTypeRef type = CFDictionaryGetValue(description,
                                              CFSTR(kIOPSTypeKey));
        if (!type || CFGetTypeID(type) != CFStringGetTypeID() ||
            !CFEqual(type, CFSTR(kIOPSInternalBatteryType)))
            continue;
        int current, maximum;
        if (dictionary_int(description, CFSTR(kIOPSCurrentCapacityKey),
                           &current) != 0 ||
            dictionary_int(description, CFSTR(kIOPSMaxCapacityKey),
                           &maximum) != 0 ||
            maximum <= 0 || current < 0 || current > maximum)
            rc = -1;
        else
            snapshot->battery_percent = current * 100 / maximum;
        break;
    }
    CFRelease(sources);
    return rc;
}

static void measure_power(struct admission_snapshot *snapshot)
{
    CFTypeRef info = IOPSCopyPowerSourcesInfo();
    if (!info) {
        snapshot->unread |= ADMISSION_ITEM_POWER | ADMISSION_ITEM_BATTERY;
        return;
    }
    CFStringRef source = IOPSGetProvidingPowerSourceType(info);
    int known = 1;
    if (source && CFEqual(source, CFSTR(kIOPMACPowerKey)))
        snapshot->power = ADMISSION_POWER_AC;
    else if (source && CFEqual(source, CFSTR(kIOPMBatteryPowerKey)))
        snapshot->power = ADMISSION_POWER_BATTERY;
    else if (source && CFEqual(source, CFSTR(kIOPMUPSPowerKey)))
        snapshot->power = ADMISSION_POWER_UPS;
    else
        known = 0;
    if (!known)
        snapshot->unread |= ADMISSION_ITEM_POWER;
    if (measure_battery(info, snapshot) != 0 ||
        (known && snapshot->power == ADMISSION_POWER_BATTERY &&
         snapshot->battery_percent < 0))
        snapshot->unread |= ADMISSION_ITEM_BATTERY;
    CFRelease(info);
}

__attribute__((format(printf, 2, 3)))
static void running_unread(struct admission_snapshot *snapshot,
                           const char *format, ...)
{
    snapshot->unread |= ADMISSION_ITEM_RUNNING_MEMORY;
    if (snapshot->running_failure[0])
        return;
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(snapshot->running_failure, sizeof(snapshot->running_failure),
              format, arguments);
    va_end(arguments);
}

static int vm_append(struct admission_snapshot *snapshot, const char *name,
                     unsigned memory_mib, int verified)
{
    struct admission_vm *vms =
        realloc(snapshot->vms, (snapshot->vm_count + 1) * sizeof(*vms));
    if (!vms)
        return -1;
    snapshot->vms = vms;
    struct admission_vm *vm = &vms[snapshot->vm_count++];
    snprintf(vm->name, sizeof(vm->name), "%s", name);
    vm->memory_mib = memory_mib;
    vm->verified = verified;
    return 0;
}

/* One profile directory of ~/.hamn other than the starting one. A VM that
 * the probe cannot verify counts as running: right after a start makes its
 * VM visible, its control socket can exist before it answers. */
static void measure_profile(const char *name,
                            struct admission_snapshot *snapshot)
{
    struct profile profile;
    char reason[PROFILE_REASON_CAP];
    int readable = profile_read_existing_reason(&profile, name, reason) == 0;
    if (!readable) {
        if (errno == ENOENT)
            return;
        memset(&profile, 0, sizeof(profile));
        snprintf(profile.name, sizeof(profile.name), "%s", name);
        if (profile_locate(&profile) != 0 ||
            profile_directory_private(&profile) != 1) {
            running_unread(snapshot, PROFILE_UNREADABLE_FORMAT, name, reason);
            return;
        }
    }
    enum vm_process_state state = vm_process_probe(&profile, NULL);
    if (state == VM_PROCESS_STALE)
        return;
    if (!readable)
        running_unread(snapshot, PROFILE_UNREADABLE_FORMAT, name, reason);
    else
        snapshot->running_memory_mib += profile.mem_mib;
    if (vm_append(snapshot, name, readable ? profile.mem_mib : 0,
                  state == VM_PROCESS_VERIFIED) != 0)
        running_unread(snapshot, "out of memory while listing running VMs");
}

static void measure_running(const char *starting,
                            struct admission_snapshot *snapshot)
{
    char root[PROFILE_PATH_CAP];
    if (!hamn_home(root, sizeof(root))) {
        running_unread(snapshot, "cannot locate ~/.hamn");
        return;
    }
    DIR *directory = opendir(root);
    if (!directory) {
        if (errno != ENOENT)
            running_unread(snapshot, "cannot list %s: %s", root,
                           strerror(errno));
        return;
    }
    for (;;) {
        errno = 0;
        struct dirent *entry = readdir(directory);
        if (!entry) {
            if (errno)
                running_unread(snapshot, "cannot list %s: %s", root,
                               strerror(errno));
            break;
        }
        if (profile_name_valid(entry->d_name) &&
            strcmp(entry->d_name, starting) != 0)
            measure_profile(entry->d_name, snapshot);
    }
    closedir(directory);
}

void admission_measure(const char *starting,
                       struct admission_snapshot *snapshot)
{
    memset(snapshot, 0, sizeof(*snapshot));
    snapshot->battery_percent = -1;

    int pressure = 0;
    if (sysctl_value("kern.memorystatus_vm_pressure_level", &pressure,
                     sizeof(pressure)) != 0 ||
        (pressure != ADMISSION_PRESSURE_NORMAL &&
         pressure != ADMISSION_PRESSURE_WARN &&
         pressure != ADMISSION_PRESSURE_CRITICAL))
        snapshot->unread |= ADMISSION_ITEM_PRESSURE;
    else
        snapshot->pressure = (enum admission_pressure)pressure;

    uint64_t bytes = 0;
    if (sysctl_value("hw.memsize", &bytes, sizeof(bytes)) != 0 || bytes < MIB)
        snapshot->unread |= ADMISSION_ITEM_TOTAL_MEMORY;
    else
        snapshot->total_memory_mib = bytes / MIB;

    double load = 0;
    if (getloadavg(&load, 1) != 1 || !isfinite(load) || load < 0)
        snapshot->unread |= ADMISSION_ITEM_LOAD;
    else
        snapshot->load_1m = load;

    int cpus = 0;
    if (sysctl_value("hw.logicalcpu", &cpus, sizeof(cpus)) != 0 || cpus < 1)
        snapshot->unread |= ADMISSION_ITEM_CPUS;
    else
        snapshot->logical_cpus = (unsigned)cpus;

    measure_power(snapshot);
    measure_running(starting, snapshot);
}

void admission_snapshot_release(struct admission_snapshot *snapshot)
{
    free(snapshot->vms);
    snapshot->vms = NULL;
    snapshot->vm_count = 0;
}

int admission_lock_acquire(struct admission_lock *lock)
{
    lock->fd = -1;
    int fd = vm_lock_file_open(".admission.lock");
    if (fd < 0)
        return -1;
    for (int waited = 0;; waited = 1) {
        if (flock(fd, LOCK_EX | LOCK_NB) == 0) {
            lock->fd = fd;
            return 0;
        }
        if (errno != EWOULDBLOCK && errno != EINTR)
            break;
        if (!waited)
            logmsg("waiting for another VM start to finish its admission "
                   "check ...");
        /* A blocking flock would miss a cancellation signal that arrives
         * just before it; polling sees the flag on the next pass. */
        if (proc_cancelled()) {
            errno = ECANCELED;
            break;
        }
        usleep(LOCK_POLL_US);
    }
    int saved = errno;
    close(fd);
    errno = saved;
    return -1;
}

void admission_lock_release(struct admission_lock *lock)
{
    if (lock->fd < 0)
        return;
    close(lock->fd);
    lock->fd = -1;
}

static const char *pressure_name(enum admission_pressure pressure)
{
    return pressure == ADMISSION_PRESSURE_CRITICAL ? "critical" :
           pressure == ADMISSION_PRESSURE_WARN ? "warn" : "normal";
}

static const char *power_name(enum admission_power power)
{
    return power == ADMISSION_POWER_BATTERY ? "battery" :
           power == ADMISSION_POWER_UPS ? "ups" : "ac";
}

/* cJSON allocation failures abort, as in core/operation.c: the refusal
 * cannot be reported without its record. */
static cJSON *made(cJSON *value)
{
    if (!value)
        abort();
    return value;
}

static void add(cJSON *object, const char *key, cJSON *value)
{
    if (!cJSON_AddItemToObject(object, key, made(value)))
        abort();
}

static void push(cJSON *array, cJSON *value)
{
    if (!cJSON_AddItemToArray(array, made(value)))
        abort();
}

static cJSON *measured_number(const struct admission_snapshot *snapshot,
                              unsigned item, double value)
{
    return snapshot->unread & item ? cJSON_CreateNull() :
                                     cJSON_CreateNumber(value);
}

static char *refusal_json(const char *profile,
                          const struct admission_snapshot *snapshot,
                          const struct admission_request *request,
                          const struct admission_limits *limits,
                          unsigned refused)
{
    cJSON *root = made(cJSON_CreateObject());
    add(root, "admission", cJSON_CreateString("refused"));
    add(root, "profile", cJSON_CreateString(profile));
    cJSON *asked = made(cJSON_CreateObject());
    add(asked, "cpus", cJSON_CreateNumber(request->cpus));
    add(asked, "memoryMiB", cJSON_CreateNumber(request->memory_mib));
    add(root, "request", asked);

    cJSON *codes = made(cJSON_CreateArray());
    for (size_t index = 0; index < COUNT(reasons); index++)
        if (refused & reasons[index].reason)
            push(codes, cJSON_CreateString(reasons[index].code));
    add(root, "reasons", codes);
    cJSON *unread = made(cJSON_CreateArray());
    for (size_t index = 0; index < COUNT(items); index++)
        if (snapshot->unread & items[index].item)
            push(unread, cJSON_CreateString(items[index].key));
    add(root, "unread", unread);

    cJSON *measured = made(cJSON_CreateObject());
    add(measured, "memoryPressure",
        snapshot->unread & ADMISSION_ITEM_PRESSURE ? cJSON_CreateNull() :
        cJSON_CreateString(pressure_name(snapshot->pressure)));
    add(measured, "totalMemoryMiB",
        measured_number(snapshot, ADMISSION_ITEM_TOTAL_MEMORY,
                        (double)snapshot->total_memory_mib));
    add(measured, "runningMemoryMiB",
        measured_number(snapshot, ADMISSION_ITEM_RUNNING_MEMORY,
                        (double)snapshot->running_memory_mib));
    add(measured, "loadAverage1m",
        measured_number(snapshot, ADMISSION_ITEM_LOAD, snapshot->load_1m));
    add(measured, "logicalCpus",
        measured_number(snapshot, ADMISSION_ITEM_CPUS,
                        snapshot->logical_cpus));
    add(measured, "powerSource",
        snapshot->unread & ADMISSION_ITEM_POWER ? cJSON_CreateNull() :
        cJSON_CreateString(power_name(snapshot->power)));
    add(measured, "batteryPercent",
        snapshot->unread & ADMISSION_ITEM_BATTERY ||
        snapshot->battery_percent < 0 ? cJSON_CreateNull() :
        cJSON_CreateNumber(snapshot->battery_percent));
    add(root, "measured", measured);

    cJSON *bounds = made(cJSON_CreateObject());
    add(bounds, "memoryReserveMiB",
        cJSON_CreateNumber((double)limits->memory_reserve_mib));
    add(bounds, "memoryPressure",
        cJSON_CreateString(pressure_name(limits->refused_pressure)));
    add(bounds, "maxLoadPerCpu",
        isfinite(limits->max_load_per_cpu) ?
        cJSON_CreateNumber(limits->max_load_per_cpu) : cJSON_CreateNull());
    add(bounds, "minBatteryPercent",
        cJSON_CreateNumber(limits->min_battery_percent));
    add(root, "limits", bounds);

    cJSON *running = made(cJSON_CreateArray());
    for (size_t index = 0; index < snapshot->vm_count; index++) {
        const struct admission_vm *vm = &snapshot->vms[index];
        cJSON *row = made(cJSON_CreateObject());
        add(row, "name", cJSON_CreateString(vm->name));
        add(row, "memoryMiB", vm->memory_mib ?
            cJSON_CreateNumber(vm->memory_mib) : cJSON_CreateNull());
        add(row, "verified", cJSON_CreateBool(vm->verified));
        push(running, row);
    }
    add(root, "runningProfiles", running);

    char *text = cJSON_PrintUnformatted(root);
    cJSON_Delete(root);
    if (!text)
        abort();
    return text;
}

struct text {
    char buffer[4096];
    size_t length;
};

__attribute__((format(printf, 2, 3)))
static void append(struct text *text, const char *format, ...)
{
    if (text->length >= sizeof(text->buffer) - 1)
        return;
    va_list arguments;
    va_start(arguments, format);
    int written = vsnprintf(text->buffer + text->length,
                            sizeof(text->buffer) - text->length, format,
                            arguments);
    va_end(arguments);
    if (written > 0)
        text->length += (size_t)written;
    if (text->length > sizeof(text->buffer) - 1)
        text->length = sizeof(text->buffer) - 1;
}

/* Each reason names the variable of its limit, so the sentence says what to
 * change; the running VMs say what to stop. */
static void refusal_message(struct text *text, const char *profile,
                            const struct admission_snapshot *snapshot,
                            const struct admission_request *request,
                            const struct admission_limits *limits,
                            unsigned refused)
{
    append(text, "refused to start the VM of profile %s: ", profile);
    const char *separator = "";
    if (refused & ADMISSION_REFUSED_UNREAD) {
        append(text, "cannot measure the host's ");
        unsigned unread = snapshot->unread & needed_items(snapshot, limits);
        const char *comma = "";
        for (size_t index = 0; index < COUNT(items); index++) {
            if (unread & items[index].item) {
                append(text, "%s%s", comma, items[index].words);
                comma = ", ";
            }
        }
        if (snapshot->running_failure[0])
            append(text, " (%s)", snapshot->running_failure);
        separator = "; ";
    }
    if (refused & ADMISSION_REFUSED_PRESSURE) {
        append(text, "%smemory pressure is %s, and starts are refused at %s "
               "or above (HAMN_ADMISSION_MEMORY_PRESSURE)", separator,
               pressure_name(snapshot->pressure),
               pressure_name(limits->refused_pressure));
        separator = "; ";
    }
    if (refused & ADMISSION_REFUSED_MEMORY) {
        append(text, "%sthe running VMs' %llu MiB and this VM's %u MiB exceed "
               "the %llu MiB of host memory less the %llu MiB reserve "
               "(HAMN_ADMISSION_MEMORY_RESERVE_MIB)", separator,
               (unsigned long long)snapshot->running_memory_mib,
               request->memory_mib,
               (unsigned long long)snapshot->total_memory_mib,
               (unsigned long long)limits->memory_reserve_mib);
        separator = "; ";
    }
    if (refused & ADMISSION_REFUSED_LOAD) {
        append(text, "%sthe 1-minute load average %.2f is %.2f per CPU on %u "
               "CPUs, above %g (HAMN_ADMISSION_MAX_LOAD_PER_CPU)", separator,
               snapshot->load_1m, snapshot->load_1m / snapshot->logical_cpus,
               snapshot->logical_cpus, limits->max_load_per_cpu);
        separator = "; ";
    }
    if (refused & ADMISSION_REFUSED_BATTERY)
        append(text, "%sthe host runs on battery at %d%%, below %u%% "
               "(HAMN_ADMISSION_MIN_BATTERY_PERCENT)", separator,
               snapshot->battery_percent, limits->min_battery_percent);
    if (snapshot->vm_count == 0) {
        append(text, ". No other Hamn VM runs");
    } else {
        append(text, ". Running VMs:");
        for (size_t index = 0; index < snapshot->vm_count; index++) {
            const struct admission_vm *vm = &snapshot->vms[index];
            append(text, "%s %s", index ? "," : "", vm->name);
            if (vm->memory_mib)
                append(text, " %u MiB", vm->memory_mib);
            else
                append(text, " (memory unknown)");
            if (!vm->verified)
                append(text, " (unverified)");
        }
    }
    append(text, ". Nothing was started");
}

int admission_admit(const char *profile, const struct admission_request *request,
                    struct admission_lock *lock, char **refusal)
{
    *refusal = NULL;
    lock->fd = -1;
    struct admission_limits limits;
    if (admission_limits_read(&limits) != 0)
        return -1;
    if (admission_lock_acquire(lock) != 0) {
        if (errno == ECANCELED)
            logerr("cancelled while waiting for the admission check of "
                   "another VM start");
        else
            logerr("cannot take the admission lock in ~/.hamn/.locks: %s",
                   strerror(errno));
        return -1;
    }
    struct admission_snapshot snapshot;
    admission_measure(profile, &snapshot);
    /* What probing the other profiles recorded is not the reason of a later
     * failure of this start. */
    log_clear_error();
    unsigned refused = admission_decide(&snapshot, request, &limits);
    if (!refused) {
        struct text message = { .length = 0 };
        append(&message, "admitted: running VMs use %llu MiB and this VM %u "
               "MiB of %llu MiB host memory (reserve %llu MiB); memory "
               "pressure %s",
               (unsigned long long)snapshot.running_memory_mib,
               request->memory_mib,
               (unsigned long long)snapshot.total_memory_mib,
               (unsigned long long)limits.memory_reserve_mib,
               pressure_name(snapshot.pressure));
        if (!(snapshot.unread & (ADMISSION_ITEM_LOAD | ADMISSION_ITEM_CPUS)))
            append(&message, "; load %.2f on %u CPUs", snapshot.load_1m,
                   snapshot.logical_cpus);
        if (!(snapshot.unread & ADMISSION_ITEM_POWER))
            append(&message, "; power %s", power_name(snapshot.power));
        /* Admitted only because no enabled limit uses these. */
        const char *comma = "; not measured: ";
        for (size_t index = 0; index < COUNT(items); index++) {
            if (snapshot.unread & items[index].item) {
                append(&message, "%s%s", comma, items[index].words);
                comma = ", ";
            }
        }
        logmsg("%s", message.buffer);
        admission_snapshot_release(&snapshot);
        return 0;
    }
    *refusal = refusal_json(profile, &snapshot, request, &limits, refused);
    struct text message = { .length = 0 };
    refusal_message(&message, profile, &snapshot, request, &limits, refused);
    logerr("%s", message.buffer);
    admission_snapshot_release(&snapshot);
    admission_lock_release(lock);
    return ADMISSION_REFUSED;
}
