/* Admission (core/admission.h): the decision at each limit's boundary, a
 * refusal for every measurement that cannot be read, the running VMs it
 * counts, and two starts that contend for the admission lock. admission.c is
 * compiled into this test with its OS calls renamed to the fake_ functions
 * below, which report the host this test describes. The profiles, their
 * vmrun probes and the lock are real. */
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <math.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/ps/IOPSKeys.h>
#include <IOKit/ps/IOPowerSources.h>

#include "cjson/cJSON.h"
#include "core/admission.h"
#include "core/control.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "util/proc.h"

/* The host the fake OS calls report; a *_fails member makes its call fail. */
static struct {
    int pressure, pressure_fails;
    uint64_t memsize;
    int memsize_fails;
    int cpus, cpus_fails;
    double load;
    int load_fails;
    int power_info_fails;
    CFStringRef source; /* NULL: no providing source */
    int battery;        /* whether an internal battery is listed */
    int capacity, max_capacity; /* -1: the key is missing */
} host;

static void host_reset(void)
{
    memset(&host, 0, sizeof(host));
    host.pressure = ADMISSION_PRESSURE_NORMAL;
    host.memsize = 8192ULL * 1024 * 1024;
    host.cpus = 8;
    host.source = CFSTR(kIOPMACPowerKey);
}

static int reply(void *old, size_t *length, const void *value, size_t size)
{
    if (!old || !length || *length < size) {
        errno = ENOMEM;
        return -1;
    }
    memcpy(old, value, size);
    *length = size;
    return 0;
}

int fake_sysctlbyname(const char *name, void *old, size_t *length, void *new,
                      size_t new_length)
{
    assert(!new && new_length == 0);
    int failed = 0;
    if (strcmp(name, "kern.memorystatus_vm_pressure_level") == 0)
        failed = host.pressure_fails;
    else if (strcmp(name, "hw.memsize") == 0)
        failed = host.memsize_fails;
    else if (strcmp(name, "hw.logicalcpu") == 0)
        failed = host.cpus_fails;
    else
        assert(!"an unexpected sysctl");
    if (failed) {
        errno = ENOENT;
        return -1;
    }
    if (strcmp(name, "hw.memsize") == 0)
        return reply(old, length, &host.memsize, sizeof(host.memsize));
    return reply(old, length,
                 name[0] == 'k' ? &host.pressure : &host.cpus, sizeof(int));
}

int fake_getloadavg(double loads[], int count)
{
    assert(count == 1);
    if (host.load_fails)
        return -1;
    loads[0] = host.load;
    return 1;
}

static void number(CFMutableDictionaryRef dictionary, CFStringRef key,
                   int value)
{
    if (value < 0)
        return;
    CFNumberRef item = CFNumberCreate(NULL, kCFNumberIntType, &value);
    CFDictionarySetValue(dictionary, key, item);
    CFRelease(item);
}

/* The power sources blob is an array of descriptions here. */
CFTypeRef fake_IOPSCopyPowerSourcesInfo(void)
{
    if (host.power_info_fails)
        return NULL;
    CFMutableArrayRef sources =
        CFArrayCreateMutable(NULL, 0, &kCFTypeArrayCallBacks);
    if (host.battery) {
        CFMutableDictionaryRef battery = CFDictionaryCreateMutable(
            NULL, 0, &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks);
        CFDictionarySetValue(battery, CFSTR(kIOPSTypeKey),
                             CFSTR(kIOPSInternalBatteryType));
        number(battery, CFSTR(kIOPSCurrentCapacityKey), host.capacity);
        number(battery, CFSTR(kIOPSMaxCapacityKey), host.max_capacity);
        CFArrayAppendValue(sources, battery);
        CFRelease(battery);
    }
    return sources;
}

CFStringRef fake_IOPSGetProvidingPowerSourceType(CFTypeRef info)
{
    (void)info;
    return host.source;
}

CFArrayRef fake_IOPSCopyPowerSourcesList(CFTypeRef info)
{
    return CFRetain(info);
}

CFDictionaryRef fake_IOPSGetPowerSourceDescription(CFTypeRef info,
                                                   CFTypeRef source)
{
    (void)info;
    return source;
}

static char root[64]; /* this test's directory */
static char hamn[160]; /* HOME/.hamn of the current case */

static const char *const limit_variables[] = {
    "HAMN_ADMISSION_MEMORY_RESERVE_MIB",
    "HAMN_ADMISSION_MEMORY_PRESSURE",
    "HAMN_ADMISSION_MAX_LOAD_PER_CPU",
    "HAMN_ADMISSION_MIN_BATTERY_PERCENT",
};

static void clear_limits(void)
{
    for (size_t i = 0; i < sizeof(limit_variables) / sizeof(limit_variables[0]);
         i++)
        assert(unsetenv(limit_variables[i]) == 0);
}

static void begin(const char *name)
{
    char home[128];
    snprintf(home, sizeof(home), "%s/%s", root, name);
    snprintf(hamn, sizeof(hamn), "%s/.hamn", home);
    assert(mkdir(home, 0700) == 0 && setenv("HOME", home, 1) == 0);
    clear_limits();
    host_reset();
}

static const char *at(const char *profile, const char *file)
{
    static char path[512];
    snprintf(path, sizeof(path), "%s/%s/%s", hamn, profile, file);
    return path;
}

static void write_file(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    size_t length = strlen(text);
    assert(fd >= 0 && write(fd, text, length) == (ssize_t)length &&
           close(fd) == 0);
}

static void create(const char *profile, unsigned memory_gib)
{
    assert(hamn_control_configure(profile, 1, memory_gib, 0, 1, -1) == 0);
}

/* A process recorded as the profile's vmrun, as vmrun records itself. It
 * lives until vmrun_kill or vmrun_exit. */
struct vmrun {
    pid_t pid;
    int alive; /* closing it lets the process exit */
};

static struct vmrun vmrun_start(const char *profile)
{
    int alive[2];
    assert(pipe(alive) == 0);
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        for (int fd = 3; fd < 256; fd++)
            if (fd != alive[0])
                close(fd);
        char byte;
        (void)read(alive[0], &byte, 1);
        _exit(0);
    }
    close(alive[0]);
    uint64_t sec, usec;
    unsigned char uuid[16];
    char hex[33], text[160];
    assert(proc_start_identity(pid, &sec, &usec) == 0 &&
           proc_executable_identity(pid, uuid) == 0);
    proc_executable_uuid_format(uuid, hex);
    snprintf(text, sizeof(text), "%d %" PRIu64 " %" PRIu64 " %s\n", pid, sec,
             usec, hex);
    write_file(at(profile, "vmrun.identity"), text);
    snprintf(text, sizeof(text), "%d\n", pid);
    write_file(at(profile, "vmrun.pid"), text);
    return (struct vmrun){ .pid = pid, .alive = alive[1] };
}

static void reap(pid_t pid, int *status)
{
    pid_t reaped;
    do {
        reaped = waitpid(pid, status, 0);
    } while (reaped < 0 && errno == EINTR);
    assert(reaped == pid);
}

/* The supervisor crashes. */
static void vmrun_kill(struct vmrun *vmrun)
{
    int status;
    assert(kill(vmrun->pid, SIGKILL) == 0);
    reap(vmrun->pid, &status);
    assert(WIFSIGNALED(status));
    close(vmrun->alive);
}

/* The supervisor ends on its own. */
static void vmrun_exit(struct vmrun *vmrun)
{
    int status;
    close(vmrun->alive);
    reap(vmrun->pid, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

/* A host with room: 16 GiB, 4 GiB of it in running VMs, a load of half a
 * CPU on 8 CPUs, on AC power; a request of 4 GiB under a 4 GiB reserve, a
 * critical pressure limit, a load limit of 0.75 per CPU and a 30% battery
 * minimum. */
struct decision {
    struct admission_snapshot snapshot;
    struct admission_request request;
    struct admission_limits limits;
};

static struct decision roomy(void)
{
    return (struct decision){
        .snapshot = {
            .pressure = ADMISSION_PRESSURE_NORMAL,
            .total_memory_mib = 16384,
            .running_memory_mib = 4096,
            .load_1m = 4.0,
            .logical_cpus = 8,
            .power = ADMISSION_POWER_AC,
            .battery_percent = -1,
        },
        .request = { .cpus = 4, .memory_mib = 4096 },
        .limits = {
            .memory_reserve_mib = 4096,
            .refused_pressure = ADMISSION_PRESSURE_CRITICAL,
            .max_load_per_cpu = 0.75,
            .min_battery_percent = 30,
        },
    };
}

static void decides(const char *name, const struct decision *decision,
                    unsigned expected)
{
    unsigned actual = admission_decide(&decision->snapshot,
                                       &decision->request,
                                       &decision->limits);
    if (actual != expected) {
        fprintf(stderr, "%s: reasons %#x, expected %#x\n", name, actual,
                expected);
        abort();
    }
}

static void decisions_follow_each_limit_at_its_boundary(void)
{
    struct decision d = roomy();
    decides("a host with room", &d, 0);

    /* 16384 - 4096 = 12288 MiB may be used by VMs. */
    d = roomy();
    d.snapshot.running_memory_mib = 8192;
    decides("memory exactly at the limit", &d, 0);
    d.snapshot.running_memory_mib = 8193;
    decides("memory 1 MiB over the limit", &d, ADMISSION_REFUSED_MEMORY);
    d.snapshot.running_memory_mib = 8191;
    decides("memory 1 MiB under the limit", &d, 0);
    d = roomy();
    d.snapshot.running_memory_mib = 0;
    d.limits.memory_reserve_mib = 16384;
    decides("a reserve of all host memory", &d, ADMISSION_REFUSED_MEMORY);
    d.limits.memory_reserve_mib = UINT32_MAX;
    decides("a reserve larger than the host", &d, ADMISSION_REFUSED_MEMORY);
    d = roomy();
    d.limits.memory_reserve_mib = 0;
    d.snapshot.running_memory_mib = 12288;
    decides("no reserve and the VMs fill the host", &d, 0);
    d.snapshot.running_memory_mib = 12289;
    decides("no reserve and the VMs exceed the host", &d,
            ADMISSION_REFUSED_MEMORY);

    struct {
        enum admission_pressure level, limit;
        unsigned expected;
    } pressures[] = {
        { ADMISSION_PRESSURE_NORMAL, ADMISSION_PRESSURE_CRITICAL, 0 },
        { ADMISSION_PRESSURE_WARN, ADMISSION_PRESSURE_CRITICAL, 0 },
        { ADMISSION_PRESSURE_CRITICAL, ADMISSION_PRESSURE_CRITICAL,
          ADMISSION_REFUSED_PRESSURE },
        { ADMISSION_PRESSURE_NORMAL, ADMISSION_PRESSURE_WARN, 0 },
        { ADMISSION_PRESSURE_WARN, ADMISSION_PRESSURE_WARN,
          ADMISSION_REFUSED_PRESSURE },
        { ADMISSION_PRESSURE_CRITICAL, ADMISSION_PRESSURE_WARN,
          ADMISSION_REFUSED_PRESSURE },
    };
    for (size_t i = 0; i < sizeof(pressures) / sizeof(pressures[0]); i++) {
        d = roomy();
        d.snapshot.pressure = pressures[i].level;
        d.limits.refused_pressure = pressures[i].limit;
        char name[64];
        snprintf(name, sizeof(name), "pressure %d under limit %d",
                 pressures[i].level, pressures[i].limit);
        decides(name, &d, pressures[i].expected);
    }

    /* 0.75 per CPU on 8 CPUs is a load of 6. */
    d = roomy();
    d.snapshot.load_1m = 6.0;
    decides("load exactly at the limit", &d, 0);
    d.snapshot.load_1m = 6.08;
    decides("load just over the limit", &d, ADMISSION_REFUSED_LOAD);
    d.snapshot.load_1m = 5.92;
    decides("load just under the limit", &d, 0);
    d.snapshot.load_1m = 1000;
    d.limits.max_load_per_cpu = INFINITY;
    decides("any load without a load limit", &d, 0);

    d = roomy();
    d.snapshot.power = ADMISSION_POWER_BATTERY;
    d.snapshot.battery_percent = 30;
    decides("battery exactly at the minimum", &d, 0);
    d.snapshot.battery_percent = 29;
    decides("battery just under the minimum", &d, ADMISSION_REFUSED_BATTERY);
    d.snapshot.battery_percent = 31;
    decides("battery just over the minimum", &d, 0);
    d.snapshot.battery_percent = 0;
    d.limits.min_battery_percent = 0;
    decides("an empty battery without a minimum", &d, 0);
    d = roomy();
    d.snapshot.battery_percent = 5;
    decides("a low battery on AC power", &d, 0);
    d.snapshot.power = ADMISSION_POWER_UPS;
    decides("a low battery on UPS power", &d, 0);

    d = roomy();
    d.snapshot.pressure = ADMISSION_PRESSURE_CRITICAL;
    d.snapshot.running_memory_mib = 12000;
    d.snapshot.load_1m = 7;
    d.snapshot.power = ADMISSION_POWER_BATTERY;
    d.snapshot.battery_percent = 10;
    decides("every limit at once", &d,
            ADMISSION_REFUSED_PRESSURE | ADMISSION_REFUSED_MEMORY |
            ADMISSION_REFUSED_LOAD | ADMISSION_REFUSED_BATTERY);

    /* An unread item refuses, its check is skipped, and the checks on read
     * items still add their reasons. */
    d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_PRESSURE;
    d.snapshot.pressure = ADMISSION_PRESSURE_CRITICAL;
    decides("unread pressure", &d, ADMISSION_REFUSED_UNREAD);
    d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_RUNNING_MEMORY;
    d.snapshot.running_memory_mib = 1u << 30;
    decides("unread running memory", &d, ADMISSION_REFUSED_UNREAD);
    d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_TOTAL_MEMORY;
    d.snapshot.total_memory_mib = 0;
    decides("unread total memory", &d, ADMISSION_REFUSED_UNREAD);
    d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_CPUS;
    d.snapshot.logical_cpus = 0;
    decides("unread CPU count", &d, ADMISSION_REFUSED_UNREAD);
    d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_LOAD;
    d.snapshot.load_1m = 1000;
    decides("unread load", &d, ADMISSION_REFUSED_UNREAD);
    d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_POWER;
    d.snapshot.power = ADMISSION_POWER_BATTERY;
    d.snapshot.battery_percent = 0;
    d.snapshot.running_memory_mib = 9000;
    decides("unread power beside too little memory", &d,
            ADMISSION_REFUSED_UNREAD | ADMISSION_REFUSED_MEMORY);
}

/* Memory pressure, total memory and running memory are always needed. The
 * load average and CPU count are needed only under a load limit; the power
 * source only under a battery minimum. */
static void only_an_item_an_enabled_limit_uses_refuses_when_unread(void)
{
    const unsigned always = ADMISSION_ITEM_PRESSURE |
                            ADMISSION_ITEM_TOTAL_MEMORY |
                            ADMISSION_ITEM_RUNNING_MEMORY;
    struct {
        const char *name;
        unsigned unread;
        double max_load_per_cpu;
        unsigned min_battery_percent;
        unsigned expected;
    } cases[] = {
        { "pressure, no optional limit", ADMISSION_ITEM_PRESSURE, INFINITY,
          0, ADMISSION_REFUSED_UNREAD },
        { "total memory, no optional limit", ADMISSION_ITEM_TOTAL_MEMORY,
          INFINITY, 0, ADMISSION_REFUSED_UNREAD },
        { "running memory, no optional limit",
          ADMISSION_ITEM_RUNNING_MEMORY, INFINITY, 0,
          ADMISSION_REFUSED_UNREAD },
        { "load under a load limit", ADMISSION_ITEM_LOAD, 0.75, 0,
          ADMISSION_REFUSED_UNREAD },
        { "load without a load limit", ADMISSION_ITEM_LOAD, INFINITY, 30, 0 },
        { "CPU count under a load limit", ADMISSION_ITEM_CPUS, 0.75, 0,
          ADMISSION_REFUSED_UNREAD },
        { "CPU count without a load limit", ADMISSION_ITEM_CPUS, INFINITY, 30,
          0 },
        { "power under a battery minimum", ADMISSION_ITEM_POWER, INFINITY, 30,
          ADMISSION_REFUSED_UNREAD },
        { "power without a battery minimum", ADMISSION_ITEM_POWER, 0.75, 0,
          0 },
        { "load, CPUs and power, no optional limit",
          ADMISSION_ITEM_LOAD | ADMISSION_ITEM_CPUS | ADMISSION_ITEM_POWER,
          INFINITY, 0, 0 },
        { "load, CPUs and power, every limit",
          ADMISSION_ITEM_LOAD | ADMISSION_ITEM_CPUS | ADMISSION_ITEM_POWER,
          0.75, 30, ADMISSION_REFUSED_UNREAD },
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        struct decision d = roomy();
        d.snapshot.unread = cases[i].unread;
        d.limits.max_load_per_cpu = cases[i].max_load_per_cpu;
        d.limits.min_battery_percent = cases[i].min_battery_percent;
        decides(cases[i].name, &d, cases[i].expected);
        if (cases[i].unread & always)
            continue;
        d.snapshot.running_memory_mib = 9000;
        decides(cases[i].name, &d,
                cases[i].expected | ADMISSION_REFUSED_MEMORY);
    }

    /* The battery charge is needed only under a battery minimum while the
     * Mac runs on battery. */
    struct decision d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_BATTERY;
    decides("an unread charge on AC power", &d, 0);
    d.snapshot.running_memory_mib = 9000;
    decides("an unread charge on AC power beside too little memory", &d,
            ADMISSION_REFUSED_MEMORY);
    d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_BATTERY;
    d.snapshot.power = ADMISSION_POWER_UPS;
    decides("an unread charge on UPS power", &d, 0);
    d.snapshot.power = ADMISSION_POWER_BATTERY;
    decides("an unread charge on battery power", &d,
            ADMISSION_REFUSED_UNREAD);
    d.limits.min_battery_percent = 0;
    decides("an unread charge on battery power without a minimum", &d, 0);
    d = roomy();
    d.snapshot.unread = ADMISSION_ITEM_POWER | ADMISSION_ITEM_BATTERY;
    d.snapshot.power = ADMISSION_POWER_AC;
    decides("no power sources under a battery minimum", &d,
            ADMISSION_REFUSED_UNREAD);
    d.limits.min_battery_percent = 0;
    decides("no power sources without a battery minimum", &d, 0);
}

static struct admission_snapshot measured(const char *starting)
{
    struct admission_snapshot snapshot;
    admission_measure(starting, &snapshot);
    return snapshot;
}

/* Which items a host change leaves unread. Under the default limits only
 * an always needed item refuses; under every limit, any item does. */
static void unread_after(const char *name, void (*change)(void),
                         unsigned item)
{
    host_reset();
    change();
    struct admission_snapshot snapshot = measured("work");
    if (snapshot.unread != item) {
        fprintf(stderr, "%s: unread %#x, expected %#x\n", name,
                snapshot.unread, item);
        abort();
    }
    struct admission_request request = { .cpus = 1, .memory_mib = 1024 };
    struct admission_limits limits;
    assert(admission_limits_read(&limits) == 0);
    unsigned always = ADMISSION_ITEM_PRESSURE | ADMISSION_ITEM_TOTAL_MEMORY |
                      ADMISSION_ITEM_RUNNING_MEMORY;
    if (admission_decide(&snapshot, &request, &limits) !=
        (item & always ? ADMISSION_REFUSED_UNREAD : 0u)) {
        fprintf(stderr, "%s: wrong decision under the default limits\n",
                name);
        abort();
    }
    limits.max_load_per_cpu = 1000;
    limits.min_battery_percent = 1;
    /* A battery charge matters only while the Mac runs on battery. */
    unsigned expected = item == ADMISSION_ITEM_BATTERY &&
                        snapshot.power != ADMISSION_POWER_BATTERY ?
                        0u : ADMISSION_REFUSED_UNREAD;
    if (admission_decide(&snapshot, &request, &limits) != expected) {
        fprintf(stderr, "%s: wrong decision under every limit\n", name);
        abort();
    }
    admission_snapshot_release(&snapshot);
}

static void pressure_fails(void) { host.pressure_fails = 1; }
static void pressure_three(void) { host.pressure = 3; }
static void pressure_zero(void) { host.pressure = 0; }
static void memsize_fails(void) { host.memsize_fails = 1; }
static void memsize_zero(void) { host.memsize = 0; }
static void cpus_fails(void) { host.cpus_fails = 1; }
static void cpus_zero(void) { host.cpus = 0; }
static void load_fails(void) { host.load_fails = 1; }
static void load_nan(void) { host.load = NAN; }
static void load_negative(void) { host.load = -1; }
static void power_info_fails(void) { host.power_info_fails = 1; }
static void no_source(void) { host.source = NULL; }
static void unknown_source(void) { host.source = CFSTR("Solar Power"); }
static void battery_power_without_battery(void)
{
    host.source = CFSTR(kIOPMBatteryPowerKey);
}
static void battery_charge_missing(void)
{
    host.battery = 1;
    host.capacity = -1;
    host.max_capacity = 100;
}
static void battery_charge_missing_on_battery(void)
{
    battery_charge_missing();
    host.source = CFSTR(kIOPMBatteryPowerKey);
}
static void battery_capacity_zero(void)
{
    host.battery = 1;
    host.capacity = 0;
    host.max_capacity = 0;
}
static void battery_over_capacity(void)
{
    host.battery = 1;
    host.capacity = 101;
    host.max_capacity = 100;
}

static void each_measurement_that_cannot_be_read_refuses(void)
{
    begin("unread");
    unread_after("pressure sysctl fails", pressure_fails,
                 ADMISSION_ITEM_PRESSURE);
    unread_after("pressure 3", pressure_three, ADMISSION_ITEM_PRESSURE);
    unread_after("pressure 0", pressure_zero, ADMISSION_ITEM_PRESSURE);
    unread_after("memsize sysctl fails", memsize_fails,
                 ADMISSION_ITEM_TOTAL_MEMORY);
    unread_after("memsize 0", memsize_zero, ADMISSION_ITEM_TOTAL_MEMORY);
    unread_after("logicalcpu sysctl fails", cpus_fails, ADMISSION_ITEM_CPUS);
    unread_after("0 CPUs", cpus_zero, ADMISSION_ITEM_CPUS);
    unread_after("getloadavg fails", load_fails, ADMISSION_ITEM_LOAD);
    unread_after("load NaN", load_nan, ADMISSION_ITEM_LOAD);
    unread_after("negative load", load_negative, ADMISSION_ITEM_LOAD);
    unread_after("no power sources", power_info_fails,
                 ADMISSION_ITEM_POWER | ADMISSION_ITEM_BATTERY);
    unread_after("no providing source", no_source, ADMISSION_ITEM_POWER);
    unread_after("an unknown source", unknown_source, ADMISSION_ITEM_POWER);
    unread_after("battery power without a battery",
                 battery_power_without_battery, ADMISSION_ITEM_BATTERY);
    unread_after("a battery without its charge", battery_charge_missing,
                 ADMISSION_ITEM_BATTERY);
    unread_after("a battery of no capacity", battery_capacity_zero,
                 ADMISSION_ITEM_BATTERY);
    unread_after("a battery over its capacity", battery_over_capacity,
                 ADMISSION_ITEM_BATTERY);
    unread_after("a battery without its charge on battery power",
                 battery_charge_missing_on_battery, ADMISSION_ITEM_BATTERY);

    host_reset();
    struct admission_snapshot snapshot = measured("work");
    assert(snapshot.unread == 0 && snapshot.power == ADMISSION_POWER_AC &&
           snapshot.battery_percent == -1 &&
           snapshot.pressure == ADMISSION_PRESSURE_NORMAL &&
           snapshot.total_memory_mib == 8192 && snapshot.logical_cpus == 8 &&
           snapshot.running_memory_mib == 0 && snapshot.vm_count == 0);
    host.source = CFSTR(kIOPMBatteryPowerKey);
    host.battery = 1;
    host.capacity = 1900;
    host.max_capacity = 4000;
    snapshot = measured("work");
    assert(snapshot.unread == 0 && snapshot.power == ADMISSION_POWER_BATTERY &&
           snapshot.battery_percent == 47);
    host.source = CFSTR(kIOPMUPSPowerKey);
    snapshot = measured("work");
    assert(snapshot.unread == 0 && snapshot.power == ADMISSION_POWER_UPS);

    /* Through admission_admit: the refusal names what it could not read. */
    host_reset();
    host.pressure_fails = 1;
    create("work", 1);
    struct admission_request request = { .cpus = 1, .memory_mib = 1024 };
    struct admission_lock lock;
    char *refusal = NULL;
    assert(admission_admit("work", &request, &lock, &refusal) ==
           ADMISSION_REFUSED && refusal && lock.fd == -1);
    assert(strstr(log_last_error(),
                  "cannot measure the host's memory pressure"));
    cJSON *value = cJSON_Parse(refusal);
    cJSON_free(refusal);
    char *text = cJSON_PrintUnformatted(cJSON_GetObjectItem(value, "reasons"));
    assert(strcmp(text, "[\"measurementFailed\"]") == 0);
    cJSON_free(text);
    text = cJSON_PrintUnformatted(cJSON_GetObjectItem(value, "unread"));
    assert(strcmp(text, "[\"memoryPressure\"]") == 0);
    cJSON_free(text);
    assert(cJSON_IsNull(cJSON_GetObjectItem(
        cJSON_GetObjectItem(value, "measured"), "memoryPressure")));
    cJSON_Delete(value);
}

/* Through admission_admit: an unread load average is no reason without a
 * load limit, yet a refusal for another reason still reports it unread. */
static void an_unread_item_no_enabled_limit_uses_is_reported_but_admitted(void)
{
    begin("unneeded");
    create("work", 1);
    host.load_fails = 1;
    struct admission_request request = { .cpus = 1, .memory_mib = 1024 };
    struct admission_lock lock;
    char *refusal = NULL;
    assert(admission_admit("work", &request, &lock, &refusal) == 0 &&
           !refusal && lock.fd >= 0);
    admission_lock_release(&lock);

    assert(setenv("HAMN_ADMISSION_MEMORY_RESERVE_MIB", "4294967295", 1) == 0);
    assert(admission_admit("work", &request, &lock, &refusal) ==
           ADMISSION_REFUSED && refusal && lock.fd == -1);
    assert(!strstr(log_last_error(), "cannot measure"));
    cJSON *value = cJSON_Parse(refusal);
    cJSON_free(refusal);
    char *text = cJSON_PrintUnformatted(cJSON_GetObjectItem(value, "reasons"));
    assert(strcmp(text, "[\"memory\"]") == 0);
    cJSON_free(text);
    text = cJSON_PrintUnformatted(cJSON_GetObjectItem(value, "unread"));
    assert(strcmp(text, "[\"loadAverage1m\"]") == 0);
    cJSON_free(text);
    assert(cJSON_IsNull(cJSON_GetObjectItem(
        cJSON_GetObjectItem(value, "measured"), "loadAverage1m")));
    cJSON_Delete(value);

    clear_limits();
    assert(setenv("HAMN_ADMISSION_MAX_LOAD_PER_CPU", "1", 1) == 0);
    assert(admission_admit("work", &request, &lock, &refusal) ==
           ADMISSION_REFUSED && refusal && lock.fd == -1);
    assert(strstr(log_last_error(),
                  "cannot measure the host's load average. "));
    value = cJSON_Parse(refusal);
    cJSON_free(refusal);
    text = cJSON_PrintUnformatted(cJSON_GetObjectItem(value, "reasons"));
    assert(strcmp(text, "[\"measurementFailed\"]") == 0);
    cJSON_free(text);
    cJSON_Delete(value);
    clear_limits();

    /* The sentence names only what refuses; the details list both. */
    host.pressure_fails = 1;
    assert(admission_admit("work", &request, &lock, &refusal) ==
           ADMISSION_REFUSED && refusal && lock.fd == -1);
    assert(strstr(log_last_error(),
                  "cannot measure the host's memory pressure. "));
    value = cJSON_Parse(refusal);
    cJSON_free(refusal);
    text = cJSON_PrintUnformatted(cJSON_GetObjectItem(value, "unread"));
    assert(strcmp(text, "[\"memoryPressure\",\"loadAverage1m\"]") == 0);
    cJSON_free(text);
    cJSON_Delete(value);
}

static int named(const struct admission_snapshot *snapshot, const char *name,
                 unsigned memory_mib, int verified)
{
    for (size_t i = 0; i < snapshot->vm_count; i++)
        if (strcmp(snapshot->vms[i].name, name) == 0)
            return snapshot->vms[i].memory_mib == memory_mib &&
                   snapshot->vms[i].verified == verified;
    return 0;
}

static void running_vms_count_until_their_vmrun_is_gone(void)
{
    begin("running");
    create("a", 2);
    create("b", 4);
    create("stopped", 8);
    assert(mkdir(at("stray", ""), 0700) == 0); /* no config.yaml */
    struct vmrun a = vmrun_start("a"), b = vmrun_start("b");

    struct admission_snapshot snapshot = measured("work");
    assert(snapshot.unread == 0 && snapshot.running_memory_mib == 6144 &&
           snapshot.vm_count == 2 && named(&snapshot, "a", 2048, 1) &&
           named(&snapshot, "b", 4096, 1));
    admission_snapshot_release(&snapshot);

    snapshot = measured("a");
    assert(snapshot.unread == 0 && snapshot.running_memory_mib == 4096 &&
           snapshot.vm_count == 1 && named(&snapshot, "b", 4096, 1));
    admission_snapshot_release(&snapshot);

    vmrun_kill(&a);
    snapshot = measured("work");
    assert(snapshot.unread == 0 && snapshot.running_memory_mib == 4096 &&
           snapshot.vm_count == 1 && named(&snapshot, "b", 4096, 1));
    admission_snapshot_release(&snapshot);

    vmrun_exit(&b);
    snapshot = measured("work");
    assert(snapshot.unread == 0 && snapshot.running_memory_mib == 0 &&
           snapshot.vm_count == 0);
    admission_snapshot_release(&snapshot);
}

/* Right after a start makes its VM visible, its vmrun can be in a state the
 * probe cannot verify; such a VM still holds its memory. */
static void a_vm_that_cannot_be_verified_counts_as_running(void)
{
    begin("unverified");
    create("spawning", 2);
    struct profile profile;
    assert(profile_read_existing(&profile, "spawning") == 0);
    int guard;
    char token[VM_SPAWN_TOKEN_HEX_SIZE];
    assert(vm_spawn_guard_create(&profile, &guard, token) == 0);
    struct admission_snapshot snapshot = measured("work");
    assert(snapshot.unread == 0 && snapshot.running_memory_mib == 2048 &&
           named(&snapshot, "spawning", 2048, 0));
    admission_snapshot_release(&snapshot);
    assert(vm_spawn_guard_release(&profile, guard, 1) == 0);
    snapshot = measured("work");
    assert(snapshot.running_memory_mib == 0 && snapshot.vm_count == 0);
    admission_snapshot_release(&snapshot);
}

static void a_running_profile_whose_configuration_cannot_be_read_refuses(void)
{
    begin("unreadable");
    create("broken", 2);
    create("work", 1);
    struct vmrun broken = vmrun_start("broken");
    write_file(at("broken", "config.yaml"), "memoryMiB: plenty\n");
    struct admission_snapshot snapshot = measured("work");
    assert(snapshot.unread == ADMISSION_ITEM_RUNNING_MEMORY &&
           named(&snapshot, "broken", 0, 1) &&
           strstr(snapshot.running_failure,
                  "cannot read the configuration of profile broken"));
    admission_snapshot_release(&snapshot);

    struct admission_request request = { .cpus = 1, .memory_mib = 1024 };
    struct admission_lock lock;
    char *refusal = NULL;
    assert(admission_admit("work", &request, &lock, &refusal) ==
           ADMISSION_REFUSED);
    assert(strstr(log_last_error(), "running VMs' memory (cannot read the "
                                    "configuration of profile broken: ") &&
           strstr(log_last_error(), "Running VMs: broken (memory unknown)"));
    cJSON *value = cJSON_Parse(refusal);
    cJSON_free(refusal);
    const cJSON *row = cJSON_GetArrayItem(
        cJSON_GetObjectItem(value, "runningProfiles"), 0);
    assert(cJSON_IsNull(cJSON_GetObjectItem(row, "memoryMiB")));
    cJSON_Delete(value);

    /* A stopped profile with a broken configuration is no VM to count. */
    vmrun_kill(&broken);
    assert(admission_admit("work", &request, &lock, &refusal) == 0 &&
           !refusal && lock.fd >= 0);
    admission_lock_release(&lock);
}

static void limits_come_from_the_environment(void)
{
    begin("limits");
    struct admission_limits limits;
    assert(admission_limits_read(&limits) == 0 &&
           limits.memory_reserve_mib == 0 &&
           limits.refused_pressure == ADMISSION_PRESSURE_CRITICAL &&
           isinf(limits.max_load_per_cpu) && limits.min_battery_percent == 0);
    setenv("HAMN_ADMISSION_MEMORY_RESERVE_MIB", "4294967295", 1);
    setenv("HAMN_ADMISSION_MEMORY_PRESSURE", "warn", 1);
    setenv("HAMN_ADMISSION_MAX_LOAD_PER_CPU", "1.5", 1);
    setenv("HAMN_ADMISSION_MIN_BATTERY_PERCENT", "100", 1);
    assert(admission_limits_read(&limits) == 0 &&
           limits.memory_reserve_mib == 4294967295ULL &&
           limits.refused_pressure == ADMISSION_PRESSURE_WARN &&
           limits.max_load_per_cpu == 1.5 && limits.min_battery_percent == 100);
    setenv("HAMN_ADMISSION_MAX_LOAD_PER_CPU", "", 1);
    assert(admission_limits_read(&limits) == 0 &&
           isinf(limits.max_load_per_cpu));

    struct {
        const char *variable, *value;
    } refused[] = {
        { "HAMN_ADMISSION_MEMORY_RESERVE_MIB", "4294967296" },
        { "HAMN_ADMISSION_MEMORY_RESERVE_MIB", "-1" },
        { "HAMN_ADMISSION_MEMORY_RESERVE_MIB", "1 GiB" },
        { "HAMN_ADMISSION_MEMORY_PRESSURE", "normal" },
        { "HAMN_ADMISSION_MEMORY_PRESSURE", "Critical" },
        { "HAMN_ADMISSION_MAX_LOAD_PER_CPU", "0" },
        { "HAMN_ADMISSION_MAX_LOAD_PER_CPU", "." },
        { "HAMN_ADMISSION_MAX_LOAD_PER_CPU", "1.2.3" },
        { "HAMN_ADMISSION_MAX_LOAD_PER_CPU", "inf" },
        { "HAMN_ADMISSION_MAX_LOAD_PER_CPU", "1e3" },
        { "HAMN_ADMISSION_MIN_BATTERY_PERCENT", "101" },
        { "HAMN_ADMISSION_MIN_BATTERY_PERCENT", "50%" },
    };
    for (size_t i = 0; i < sizeof(refused) / sizeof(refused[0]); i++) {
        clear_limits();
        assert(setenv(refused[i].variable, refused[i].value, 1) == 0);
        log_clear_error();
        if (admission_limits_read(&limits) != -1 ||
            !strstr(log_last_error(), refused[i].variable)) {
            fprintf(stderr, "%s=%s was accepted\n", refused[i].variable,
                    refused[i].value);
            abort();
        }
    }
}

static long long now_ms(void)
{
    struct timespec now;
    assert(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
    return (long long)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

/* Reads fd into text until it holds needle or fd ends; aborts after 10 s.
 * Returns whether needle came. other, when not -1, must not become readable
 * first. */
static int read_until(int fd, char *text, size_t capacity, const char *needle,
                      int other)
{
    long long deadline = now_ms() + 10000;
    size_t length = strlen(text);
    while (!strstr(text, needle)) {
        struct pollfd watched[2] = {
            { .fd = fd, .events = POLLIN },
            { .fd = other, .events = POLLIN },
        };
        long long left = deadline - now_ms();
        assert(left > 0);
        int ready = poll(watched, other >= 0 ? 2 : 1, (int)left);
        if (ready < 0 && errno == EINTR)
            continue;
        assert(ready > 0);
        if (other >= 0 && watched[1].revents)
            return 0;
        ssize_t count = read(fd, text + length, capacity - 1 - length);
        assert(count >= 0);
        if (count == 0)
            return 0;
        length += (size_t)count;
        text[length] = '\0';
    }
    return 1;
}

/* A child that calls admission_admit for profile and writes its return value
 * and refusal to result; its stdout, where logmsg writes, is output. */
struct contender {
    pid_t pid;
    int output, result;
};

static struct contender contend(const char *profile, int go)
{
    int output[2], result[2];
    assert(pipe(output) == 0 && pipe(result) == 0);
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        assert(dup2(output[1], STDOUT_FILENO) == STDOUT_FILENO);
        /* Only this test's end of go: an aborted test then ends A too. */
        for (int fd = 3; fd < 256; fd++)
            if (fd != result[1] && fd != go)
                close(fd);
        struct admission_request request = { .cpus = 2, .memory_mib = 4096 };
        struct admission_lock lock;
        char *refusal = NULL;
        int rc = admission_admit(profile, &request, &lock, &refusal);
        dprintf(result[1], "%d\n%s", rc, refusal ? refusal : "");
        close(result[1]);
        if (rc == 0 && go >= 0) {
            char byte = 0;
            (void)read(go, &byte, 1);
            admission_lock_release(&lock);
        }
        _exit(0);
    }
    close(output[1]);
    close(result[1]);
    return (struct contender){ .pid = pid, .output = output[0],
                               .result = result[0] };
}

static void finished(struct contender *contender, char *result,
                     size_t capacity)
{
    result[0] = '\0';
    (void)read_until(contender->result, result, capacity, "\x01", -1);
    int status;
    reap(contender->pid, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    close(contender->output);
    close(contender->result);
}

/* A starts and holds the lock as a start does until its VM is visible. B
 * starts meanwhile, must wait for the lock, and then counts A's VM: 8 GiB
 * less a 2 GiB reserve takes one 4 GiB VM, not two. */
static void two_concurrent_starts_cannot_pass_on_the_same_headroom(void)
{
    begin("race");
    assert(setenv("HAMN_ADMISSION_MEMORY_RESERVE_MIB", "2048", 1) == 0);
    create("a", 4);
    create("b", 4);
    int go[2];
    assert(pipe(go) == 0);
    struct contender a = contend("a", go[0]);
    close(go[0]);
    char text[8192] = "";
    assert(read_until(a.result, text, sizeof(text), "\n", -1) &&
           strcmp(text, "0\n") == 0);

    struct contender b = contend("b", -1);
    char waiting[4096] = "";
    if (!read_until(b.output, waiting, sizeof(waiting),
                    "waiting for another VM start to finish its admission "
                    "check", b.result)) {
        fprintf(stderr, "B decided while A held the admission lock\n");
        abort();
    }

    struct vmrun vm = vmrun_start("a");
    assert(write(go[1], "g", 1) == 1);
    close(go[1]);
    finished(&a, text, sizeof(text));
    finished(&b, text, sizeof(text));

    char *json = strchr(text, '\n');
    assert(strncmp(text, "1\n", 2) == 0 && json);
    cJSON *value = cJSON_Parse(json + 1);
    assert(value);
    char *reasons = cJSON_PrintUnformatted(cJSON_GetObjectItem(value, "reasons"));
    char *running = cJSON_PrintUnformatted(
        cJSON_GetObjectItem(value, "runningProfiles"));
    const cJSON *measured_value = cJSON_GetObjectItem(value, "measured");
    assert(strcmp(reasons, "[\"memory\"]") == 0);
    assert(strcmp(running,
                  "[{\"name\":\"a\",\"memoryMiB\":4096,\"verified\":true}]") == 0);
    assert(cJSON_GetObjectItem(measured_value, "runningMemoryMiB")->valuedouble ==
           4096);
    cJSON_free(reasons);
    cJSON_free(running);
    cJSON_Delete(value);
    vmrun_kill(&vm);
}

/* A start that waits behind another can be cancelled. The waiting start
 * polls, so a signal that arrives before it would block still ends it. */
static void a_cancelled_wait_ends(void)
{
    begin("cancel");
    struct admission_lock held;
    assert(admission_lock_acquire(&held) == 0);
    int output[2];
    assert(pipe(output) == 0);
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        assert(dup2(output[1], STDOUT_FILENO) == STDOUT_FILENO);
        /* Not the parent's lock: an aborted test then frees this child. */
        for (int fd = 3; fd < 256; fd++)
            close(fd);
        assert(proc_cancel_install() == 0);
        struct admission_lock lock;
        int rc = admission_lock_acquire(&lock);
        _exit(rc == -1 && errno == ECANCELED && lock.fd == -1 ? 0 : 1);
    }
    close(output[1]);
    char text[4096] = "";
    assert(read_until(output[0], text, sizeof(text), "waiting for another",
                      -1));
    assert(kill(pid, SIGTERM) == 0);
    assert(!read_until(output[0], text, sizeof(text), "\x01", -1));
    int status;
    reap(pid, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    close(output[0]);
    admission_lock_release(&held);
}

/* "admission" is a valid profile name: its lifecycle lock must not be the
 * admission lock, or its own start would wait for itself. */
static void the_admission_lock_is_no_profile_lifecycle_lock(void)
{
    begin("collision");
    struct vm_lifecycle_lock lifecycle;
    assert(vm_lifecycle_lock_acquire("admission", &lifecycle) == 0);
    int output[2];
    assert(pipe(output) == 0);
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        assert(dup2(output[1], STDOUT_FILENO) == STDOUT_FILENO);
        for (int fd = 3; fd < 256; fd++)
            close(fd);
        struct admission_lock lock;
        _exit(admission_lock_acquire(&lock) == 0 ? 0 : 1);
    }
    close(output[1]);
    char text[4096] = "";
    if (read_until(output[0], text, sizeof(text), "waiting", -1)) {
        kill(pid, SIGKILL);
        fprintf(stderr, "the admission lock waited for a lifecycle lock\n");
        abort();
    }
    int status;
    reap(pid, &status);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    close(output[0]);
    vm_lifecycle_lock_release(&lifecycle);
}

int main(void)
{
    snprintf(root, sizeof(root), "/tmp/hamn-admission-XXXXXX");
    assert(mkdtemp(root));
    decisions_follow_each_limit_at_its_boundary();
    only_an_item_an_enabled_limit_uses_refuses_when_unread();
    each_measurement_that_cannot_be_read_refuses();
    an_unread_item_no_enabled_limit_uses_is_reported_but_admitted();
    running_vms_count_until_their_vmrun_is_gone();
    a_vm_that_cannot_be_verified_counts_as_running();
    a_running_profile_whose_configuration_cannot_be_read_refuses();
    limits_come_from_the_environment();
    two_concurrent_starts_cannot_pass_on_the_same_headroom();
    a_cancelled_wait_ends();
    the_admission_lock_is_no_profile_lifecycle_lock();
    char command[128];
    snprintf(command, sizeof(command), "rm -rf '%s'", root);
    assert(system(command) == 0);
    printf("admission tests passed\n");
    return 0;
}
