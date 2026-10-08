#include "core/control.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"
#include "util/proc.h"

/* Declarative profile configuration: `vm apply` makes config.yaml equal to a
 * profile definition file. The definition is the whole desired configuration;
 * this module decides what that takes (create, configure or nothing) and
 * writes it under the same locks as every other profile mutation. It never
 * starts, stops or signals a VM. */

/* The return values of hamn_control_apply; core/control.h states the
 * contract of each. */
enum {
    APPLY_OK = 0,
    APPLY_FAILED = 1,
    APPLY_INVALID = 2,
    APPLY_CONFLICT = 4,
    APPLY_UNCONFIRMED = 5,
    APPLY_CANCELLED = 130,
};

enum apply_action {
    ACTION_NONE,
    ACTION_CREATE,
    ACTION_CONFIGURE,
};

struct apply_plan {
    enum apply_action action;
    unsigned changes; /* profile_diff() of stored and desired; 0 unless configure */
};

/* Reads the regular file at path into text, which holds capacity bytes, and
 * NUL-terminates it. A file of capacity bytes or more is refused. Returns 0,
 * or -1 after logging why. */
static int definition_read(const char *path, char *text, size_t capacity,
                           size_t *length)
{
    /* O_NONBLOCK: opening a FIFO must not wait for a writer. */
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NONBLOCK);
    if (fd < 0) {
        logerr("cannot read the profile definition %s: %s", path,
               strerror(errno));
        return -1;
    }
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode)) {
        logerr("the profile definition %s is not a regular file", path);
        close(fd);
        return -1;
    }
    size_t used = 0;
    while (used < capacity) {
        ssize_t count = read(fd, text + used, capacity - used);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0) {
            logerr("cannot read the profile definition %s: %s", path,
                   strerror(errno));
            close(fd);
            return -1;
        }
        if (count == 0)
            break;
        used += (size_t)count;
    }
    close(fd);
    if (used == capacity) {
        logerr("the profile definition %s is larger than %d bytes", path,
               PROFILE_DEFINITION_CAP);
        return -1;
    }
    text[used] = '\0';
    *length = used;
    return 0;
}

#define CONFIG_FILE "config.yaml"
#define CONFIG_TEMPORARY_PREFIX CONFIG_FILE ".tmp."

static int has_prefix(const char *name, const char *prefix)
{
    return strncmp(name, prefix, strlen(prefix)) == 0;
}

/* What Hamn itself can leave in a profile directory before the profile has
 * a config.yaml. A configuration write that was interrupted leaves its
 * temporary file. A first `vm start` writes its operation record, and then
 * the state file and the port forwarding locks of its stale-state cleanup,
 * before it saves the configuration, and can be refused or interrupted at
 * any point of that. None of these is a disk, an archive, or the pid,
 * identity, spawn guard or control socket of a VM. */
static int precedes_configuration(const char *name)
{
    return has_prefix(name, CONFIG_TEMPORARY_PREFIX) ||
           strcmp(name, "operation.json") == 0 ||
           has_prefix(name, "operation.json.tmp.") ||
           strcmp(name, "state.json") == 0 ||
           has_prefix(name, "state.json.tmp.") ||
           strcmp(name, "port-forward-operations.lock") == 0 ||
           strcmp(name, "port-forwards.lock") == 0;
}

/* What a profile directory holds, for one in which config.yaml could not be
 * opened a moment ago. */
enum directory_content {
    DIRECTORY_UNREADABLE = -1,
    DIRECTORY_FOREIGN,     /* an entry that precedes no configuration */
    DIRECTORY_AWAITS,      /* nothing but what precedes a configuration */
    DIRECTORY_CONFIGURED,  /* config.yaml: it was written meanwhile */
};

/* With remove_temporaries, which requires the lifecycle lock that every
 * configuration write holds, the temporary files met are unlinked: nothing
 * else would ever remove them. errno is set for DIRECTORY_UNREADABLE. */
static enum directory_content directory_scan(const char *path,
                                             int remove_temporaries)
{
    DIR *directory = opendir(path);
    if (!directory)
        return DIRECTORY_UNREADABLE;
    enum directory_content content = DIRECTORY_AWAITS;
    for (;;) {
        errno = 0;
        struct dirent *entry = readdir(directory);
        if (!entry) {
            if (errno)
                content = DIRECTORY_UNREADABLE;
            break;
        }
        if (strcmp(entry->d_name, ".") == 0 ||
            strcmp(entry->d_name, "..") == 0)
            continue;
        if (strcmp(entry->d_name, CONFIG_FILE) == 0) {
            content = DIRECTORY_CONFIGURED;
            break;
        }
        if (!precedes_configuration(entry->d_name)) {
            content = DIRECTORY_FOREIGN;
            break;
        }
        if (remove_temporaries &&
            has_prefix(entry->d_name, CONFIG_TEMPORARY_PREFIX) &&
            unlinkat(dirfd(directory), entry->d_name, 0) != 0 &&
            errno != ENOENT) {
            content = DIRECTORY_UNREADABLE;
            break;
        }
    }
    int saved = errno;
    closedir(directory);
    errno = saved;
    return content;
}

/* "cpus, mounts" for the settings of a profile_diff() result. */
static const char *changes_text(unsigned changes, char *text, size_t capacity)
{
    size_t used = 0;
    text[0] = '\0';
    for (int setting = 0; setting < PROFILE_SETTING_COUNT; setting++) {
        if (!(changes & 1u << setting))
            continue;
        int written = snprintf(text + used, capacity - used, "%s%s",
                               used ? ", " : "", profile_setting_key(setting));
        if (written < 0 || (size_t)written >= capacity - used)
            break;
        used += (size_t)written;
    }
    return text;
}

/* A directory that holds something other than what precedes a
 * configuration: a leftover disk, the archives of `vm diagnostics`. */
static int refuse_foreign_directory(const struct profile *desired)
{
    logerr("%s holds files but no config.yaml; it is not adopted as "
           "profile %s", desired->dir, desired->name);
    return APPLY_CONFLICT;
}

/* How often an evaluation starts over because the profile was created or
 * removed while it looked. */
#define EVALUATE_ATTEMPTS 3

/* What it takes to give the stored profile the settings of desired. Reads
 * only, and takes no lock itself. Returns APPLY_OK with *plan filled, and
 * *current filled unless the plan is to create; otherwise the return value
 * of the apply, after logging why.
 *
 * settled says that the answer is the caller's last word: it holds the
 * lifecycle lock, under which no other call creates or removes the profile,
 * or it is a dry run, which takes no lock at all. Without it, a directory
 * that is no profile is not refused but planned as a creation, and the
 * caller decides again under the lock: at this moment another call can be
 * in the middle of creating the profile there. */
static int apply_evaluate(const struct profile *desired,
                          struct profile *current, struct apply_plan *plan,
                          int settled)
{
    for (int attempt = 1;; attempt++) {
        plan->action = ACTION_NONE;
        plan->changes = 0;
        struct stat status;
        if (lstat(desired->dir, &status) != 0) {
            if (errno != ENOENT) {
                logerr("cannot inspect profile %s: %s", desired->name,
                       strerror(errno));
                return APPLY_FAILED;
            }
            plan->action = ACTION_CREATE;
            return APPLY_OK;
        }
        /* Only `vm start` brings a deleted profile back: its disk and
         * Docker data come back with it, and a configuration write must
         * not do that. */
        char marker[PROFILE_PATH_CAP];
        if (!profile_path(desired, "deleted", marker, sizeof(marker))) {
            logerr("the path of profile %s is too long", desired->name);
            return APPLY_FAILED;
        }
        if (lstat(marker, &status) == 0) {
            logerr("profile %s is deleted; vm start --profile %s restores "
                   "it with its disk, or use another name", desired->name,
                   desired->name);
            return APPLY_CONFLICT;
        }
        if (errno != ENOENT) {
            logerr("cannot inspect profile %s: %s", desired->name,
                   strerror(errno));
            return APPLY_FAILED;
        }
        char reason[PROFILE_REASON_CAP];
        if (profile_read_existing_reason(current, desired->name,
                                         reason) == 0) {
            if (desired->disk_gib < current->disk_gib) {
                logerr("disk size cannot shrink (current: %u GiB); set "
                       "spec.diskGiB to %u or more", current->disk_gib,
                       current->disk_gib);
                return APPLY_CONFLICT;
            }
            plan->changes = profile_diff(current, desired);
            plan->action = plan->changes ? ACTION_CONFIGURE : ACTION_NONE;
            return APPLY_OK;
        }
        if (errno != ENOENT) {
            logerr(PROFILE_UNREADABLE_FORMAT, desired->name, reason);
            return APPLY_FAILED;
        }
        /* A directory without config.yaml. One that holds nothing, or only
         * what an interrupted or refused creation leaves, has nothing to
         * adopt and becomes the profile. */
        switch (directory_scan(desired->dir, 0)) {
        case DIRECTORY_AWAITS:
            plan->action = ACTION_CREATE;
            return APPLY_OK;
        case DIRECTORY_CONFIGURED:
            /* Written between the read and the scan: read it. */
            if (attempt < EVALUATE_ATTEMPTS)
                continue;
            logerr("profile %s is being changed; nothing was written",
                   desired->name);
            return APPLY_FAILED;
        case DIRECTORY_UNREADABLE:
            /* Gone between the two: a creation that failed removed it. */
            if (errno == ENOENT && attempt < EVALUATE_ATTEMPTS)
                continue;
            logerr("cannot inspect profile %s: %s", desired->name,
                   strerror(errno));
            return APPLY_FAILED;
        case DIRECTORY_FOREIGN:
            break;
        }
        if (settled)
            return refuse_foreign_directory(desired);
        plan->action = ACTION_CREATE;
        return APPLY_OK;
    }
}

/* The value of a number or boolean setting. Other settings report only their
 * key: a hook command or the Docker daemon settings can hold a credential. */
static cJSON *setting_value(const struct profile *profile,
                            enum profile_setting setting)
{
    switch (setting) {
    case PROFILE_SETTING_CPUS:
        return cJSON_CreateNumber(profile->cpus);
    case PROFILE_SETTING_MEMORY_MIB:
        return cJSON_CreateNumber(profile->mem_mib);
    case PROFILE_SETTING_DISK_GIB:
        return cJSON_CreateNumber(profile->disk_gib);
    case PROFILE_SETTING_MOUNT_HOME:
        return cJSON_CreateBool(profile->mount_home);
    case PROFILE_SETTING_HOME_READ_ONLY:
        return cJSON_CreateBool(profile->home_read_only);
    case PROFILE_SETTING_MOUNT_INOTIFY:
        return cJSON_CreateBool(profile->mount_inotify);
    case PROFILE_SETTING_ROSETTA:
        return cJSON_CreateBool(profile->rosetta);
    case PROFILE_SETTING_NESTED_VIRTUALIZATION:
        return cJSON_CreateBool(profile->nested_virtualization);
    case PROFILE_SETTING_SSH_AGENT:
        return cJSON_CreateBool(profile->ssh_agent);
    default:
        return NULL;
    }
}

static int setting_has_value(enum profile_setting setting)
{
    return setting != PROFILE_SETTING_DOCKER_DAEMON_JSON &&
           setting != PROFILE_SETTING_MOUNTS &&
           setting != PROFILE_SETTING_PROVISION;
}

static cJSON *changes_json(const struct profile *desired,
                           const struct profile *current, unsigned changes)
{
    cJSON *list = cJSON_CreateArray();
    if (!list)
        return NULL;
    for (int setting = 0; setting < PROFILE_SETTING_COUNT; setting++) {
        if (!(changes & 1u << setting))
            continue;
        cJSON *change = cJSON_CreateObject();
        if (!change || !cJSON_AddItemToArray(list, change)) {
            cJSON_Delete(change);
            goto fail;
        }
        if (!cJSON_AddStringToObject(change, "key",
                                     profile_setting_key(setting)))
            goto fail;
        if (!setting_has_value(setting))
            continue;
        cJSON *from = setting_value(current, setting);
        if (!from || !cJSON_AddItemToObject(change, "from", from)) {
            cJSON_Delete(from);
            goto fail;
        }
        cJSON *to = setting_value(desired, setting);
        if (!to || !cJSON_AddItemToObject(change, "to", to)) {
            cJSON_Delete(to);
            goto fail;
        }
    }
    return list;
fail:
    cJSON_Delete(list);
    return NULL;
}

/* Stores the result of an apply in *result and logs one line for the
 * operation log. Returns APPLY_OK, or APPLY_FAILED without a result. */
static int apply_report(const struct profile *desired,
                        const struct profile *current,
                        const struct apply_plan *plan, int dry_run,
                        char **result)
{
    static const char *const actions[] = {
        [ACTION_NONE] = "none",
        [ACTION_CREATE] = "create",
        [ACTION_CONFIGURE] = "configure",
    };
    cJSON *value = cJSON_CreateObject();
    if (!value ||
        !cJSON_AddStringToObject(value, "profile", desired->name) ||
        !cJSON_AddStringToObject(value, "action", actions[plan->action]))
        goto fail;
    cJSON *changes = changes_json(desired, current, plan->changes);
    if (!changes || !cJSON_AddItemToObject(value, "changes", changes)) {
        cJSON_Delete(changes);
        goto fail;
    }
    if (!cJSON_AddBoolToObject(value, "dryRun", dry_run))
        goto fail;
    *result = cJSON_PrintUnformatted(value);
    if (!*result)
        goto fail;
    cJSON_Delete(value);
    char keys[256];
    switch (plan->action) {
    case ACTION_CREATE:
        logmsg("profile %s: %s", desired->name,
               dry_run ? "would be created" : "created");
        break;
    case ACTION_CONFIGURE:
        logmsg("profile %s: %s (%s)", desired->name,
               dry_run ? "would be configured" : "configured",
               changes_text(plan->changes, keys, sizeof(keys)));
        break;
    default:
        logmsg("profile %s: unchanged", desired->name);
    }
    return APPLY_OK;
fail:
    cJSON_Delete(value);
    logerr("cannot build the result of vm apply");
    return APPLY_FAILED;
}

/* Writes config.yaml. made_directory says that this call created the profile
 * directory. A failed write can have replaced the file already
 * (fs_write_file_atomic fails after its rename when the directory cannot be
 * synchronized), so the file is read back before the failure is classified. */
static int apply_save(const struct profile *desired, int made_directory)
{
    if (profile_save(desired) == 0)
        return APPLY_OK;
    int saved = errno;
    struct profile stored;
    char reason[PROFILE_REASON_CAP];
    if (profile_read_existing_reason(&stored, desired->name, reason) == 0) {
        if (profile_diff(&stored, desired) == 0) {
            logerr("the settings of profile %s were written but not "
                   "confirmed durable: %s; run vm apply again",
                   desired->name, strerror(saved));
            return APPLY_UNCONFIRMED;
        }
    } else if (errno != ENOENT) {
        logerr("cannot save settings: %s; the configuration of profile %s "
               "cannot be read back: %s", strerror(saved), desired->name,
               reason);
        return APPLY_UNCONFIRMED;
    }
    if (made_directory)
        (void)rmdir(desired->dir);
    logerr("cannot save settings: %s", strerror(saved));
    return APPLY_FAILED;
}

/* Creates the profile. The caller holds both locks and has found, under
 * them, no directory or one that awaits its configuration. No VM can belong
 * to such a profile: its pid, identity, spawn guard and control socket are
 * entries that the scan refuses. */
static int apply_create(const struct profile *desired)
{
    int made_directory = mkdir(desired->dir, 0700) == 0;
    if (made_directory)
        return apply_save(desired, 1);
    if (errno != EEXIST) {
        logerr("cannot create profile %s: %s", desired->name, strerror(errno));
        return APPLY_FAILED;
    }
    /* The directory was there. This scan removes the temporary file of an
     * earlier, interrupted write. It also refuses what a writer that takes
     * no lock put there since the evaluation, such as `vm diagnostics`. */
    switch (directory_scan(desired->dir, 1)) {
    case DIRECTORY_AWAITS:
        return apply_save(desired, 0);
    case DIRECTORY_FOREIGN:
        return refuse_foreign_directory(desired);
    case DIRECTORY_CONFIGURED:
        logerr("profile %s is being changed; nothing was written",
               desired->name);
        return APPLY_FAILED;
    default:
        logerr("cannot inspect profile %s: %s", desired->name,
               strerror(errno));
        return APPLY_FAILED;
    }
}

static int apply_configure(const struct profile *desired,
                           const struct profile *current,
                           const struct apply_plan *plan)
{
    char keys[256];
    switch (vm_process_probe(current, NULL)) {
    case VM_PROCESS_STALE:
        return apply_save(desired, 0);
    case VM_PROCESS_VERIFIED:
        logerr("VM must be stopped before changing settings: %s",
               changes_text(plan->changes, keys, sizeof(keys)));
        return APPLY_CONFLICT;
    default:
        /* Not a state that stopping resolves: never report it as one. */
        logerr("cannot verify the VM process of profile %s; settings were "
               "not changed", desired->name);
        return APPLY_FAILED;
    }
}

int hamn_control_apply(const char *profile_name, const char *path, int dry_run,
                       char **result)
{
    if (result)
        *result = NULL;
    if (!result || !path || !path[0] || !profile_name_valid(profile_name)) {
        logerr("vm apply requires a valid profile name and a definition file");
        return APPLY_INVALID;
    }
    char text[PROFILE_DEFINITION_CAP + 1];
    size_t length = 0;
    if (definition_read(path, text, sizeof(text), &length) != 0)
        return APPLY_INVALID;
    struct profile desired, current;
    char reason[PROFILE_REASON_CAP];
    if (profile_definition_parse(text, length, &desired, reason) != 0) {
        logerr("invalid profile definition: %s", reason);
        return APPLY_INVALID;
    }
    if (strcmp(desired.name, profile_name) != 0) {
        logerr("the definition names profile %s but --profile is %s",
               desired.name, profile_name);
        return APPLY_INVALID;
    }
    if (!profile_storable(&desired)) {
        logerr("invalid profile definition: %s", errno == EOVERFLOW ?
               "its settings do not fit in config.yaml, which holds 65536 "
               "bytes" :
               "a setting holds a character that config.yaml cannot store");
        return APPLY_INVALID;
    }
    if (profile_locate(&desired) != 0) {
        logerr("cannot locate profile %s: %s", desired.name, strerror(errno));
        return APPLY_FAILED;
    }

    struct apply_plan plan;
    int rc = apply_evaluate(&desired, &current, &plan, dry_run);
    if (rc != APPLY_OK)
        return rc;
    /* Equal settings need no lock: a repeated apply succeeds while the VM
     * runs and does not queue behind another operation on the profile. */
    if (dry_run || plan.action == ACTION_NONE)
        return apply_report(&desired, &current, &plan, dry_run, result);

    struct vm_lifecycle_lock lifecycle;
    if (vm_lifecycle_lock_acquire(desired.name, &lifecycle) != 0) {
        logerr("cannot lock the %s profile lifecycle", desired.name);
        return APPLY_FAILED;
    }
    int mutation = -1;
    /* The wait for the lifecycle lock is not interruptible. A call whose
     * deadline passed meanwhile must not write. */
    if (proc_cancelled()) {
        logerr("cancelled before changing settings");
        rc = APPLY_CANCELLED;
        goto out;
    }
    /* The profile can have changed during that wait, and what the first
     * look left open is decided now. */
    rc = apply_evaluate(&desired, &current, &plan, 1);
    if (rc != APPLY_OK)
        goto out;
    if (plan.action == ACTION_NONE) {
        rc = apply_report(&desired, &current, &plan, 0, result);
        goto out;
    }
    mutation = profile_mutation_lock(&desired);
    if (mutation < 0) {
        logerr("another %s profile mutation is running", desired.name);
        rc = APPLY_FAILED;
        goto out;
    }
    rc = plan.action == ACTION_CREATE ? apply_create(&desired) :
        apply_configure(&desired, &current, &plan);
    /* Past the write, a missing result must not read as "unchanged". */
    if (rc == APPLY_OK &&
        apply_report(&desired, &current, &plan, 0, result) != APPLY_OK)
        rc = APPLY_UNCONFIRMED;
out:
    if (mutation >= 0)
        profile_mutation_unlock(mutation);
    vm_lifecycle_lock_release(&lifecycle);
    return rc;
}
