#include "core/kubeconfig.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/log.h"
#include "util/fs.h"
#include "util/proc.h"

#define KUBECONFIG_CAP (512U * 1024U)
#define KUBE_CONTEXT_CAP 128

static int global_kubeconfig_path(char output[PATH_MAX])
{
    const char *home = getenv("HOME");
    if (!home || home[0] != '/' || strchr(home, ':')) {
        errno = EINVAL;
        return -1;
    }
    int written = snprintf(output, PATH_MAX, "%s/.kube/config", home);
    if (written < 0 || written >= PATH_MAX) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

static int owned_directory(const char *path)
{
    struct stat status;
    if (lstat(path, &status) != 0 || !S_ISDIR(status.st_mode) ||
        status.st_uid != geteuid()) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

static int owned_regular_file(const char *path)
{
    struct stat status;
    if (lstat(path, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_uid != geteuid()) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

static int ensure_global_kubeconfig(const char *path)
{
    char directory[PATH_MAX];
    int written = snprintf(directory, sizeof(directory), "%s", path);
    if (written < 0 || written >= (int)sizeof(directory)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    char *slash = strrchr(directory, '/');
    if (!slash || slash == directory) {
        errno = EINVAL;
        return -1;
    }
    *slash = '\0';
    if (fs_mkdirs(directory, 0700) != 0 || owned_directory(directory) != 0)
        return -1;

    if (lstat(path, &(struct stat){0}) == 0)
        return owned_regular_file(path);
    if (errno != ENOENT)
        return -1;
    return fs_write_file_atomic(path, "apiVersion: v1\nkind: Config\n",
                                strlen("apiVersion: v1\nkind: Config\n"),
                                0600);
}

static int global_kubeconfig_lock(const char *global)
{
    char directory[PATH_MAX], lock_path[PATH_MAX];
    int written = snprintf(directory, sizeof(directory), "%s", global);
    if (written < 0 || written >= (int)sizeof(directory)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    char *slash = strrchr(directory, '/');
    if (!slash || slash == directory) {
        errno = EINVAL;
        return -1;
    }
    *slash = '\0';
    written = snprintf(lock_path, sizeof(lock_path), "%s/.hamn-config.lock",
                       directory);
    if (written < 0 || written >= (int)sizeof(lock_path)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    int fd = open(lock_path, O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (fd < 0)
        return -1;
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_uid != geteuid() || (status.st_mode & 077) != 0) {
        int saved = errno ? errno : EINVAL;
        close(fd);
        errno = saved;
        return -1;
    }
    while (flock(fd, LOCK_EX) != 0) {
        if (errno == EINTR)
            continue;
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    return fd;
}

static void global_kubeconfig_unlock(int fd)
{
    if (fd < 0)
        return;
    (void)flock(fd, LOCK_UN);
    (void)close(fd);
}

/* 1=read, 0=absent, -1=unsafe or malformed filesystem input. */
static int read_owned_regular_file(const char *path, char *output, size_t cap)
{
    if (!output || cap < 2) {
        errno = EINVAL;
        return -1;
    }
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return errno == ENOENT ? 0 : -1;
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_uid != geteuid() || status.st_size < 0 ||
        (unsigned long long)status.st_size >= cap) {
        int saved = errno ? errno : EINVAL;
        close(fd);
        errno = saved;
        return -1;
    }
    size_t offset = 0;
    while (offset < cap - 1) {
        ssize_t count = read(fd, output + offset, cap - 1 - offset);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            int saved = errno;
            close(fd);
            errno = saved;
            return -1;
        }
        if (count == 0)
            break;
        offset += (size_t)count;
    }
    if (close(fd) != 0)
        return -1;
    output[offset] = '\0';
    return 1;
}

static int context_name(const struct profile *profile,
                        char output[KUBE_CONTEXT_CAP])
{
    return profile_docker_context_name(profile, output, KUBE_CONTEXT_CAP);
}

/* The marker lives under the Hamn root rather than the profile directory so a
 * hard profile delete cannot turn a previously Hamn-owned context into an
 * indistinguishable foreign collision on the next create. `uninstall` removes
 * this root together with all other Hamn ownership metadata. */
static int owner_marker_path(const struct profile *profile,
                             char output[PATH_MAX], int create_parent)
{
    char root[PROFILE_PATH_CAP], directory[PATH_MAX];
    if (!profile || !profile_name_valid(profile->name) ||
        !hamn_home(root, sizeof(root))) {
        errno = EINVAL;
        return -1;
    }
    int written = snprintf(directory, sizeof(directory), "%s/.kube-contexts",
                           root);
    if (written < 0 || written >= (int)sizeof(directory)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    if (create_parent &&
        (fs_mkdirs(directory, 0700) != 0 || owned_directory(directory) != 0))
        return -1;
    written = snprintf(output, PATH_MAX, "%s/%s", directory, profile->name);
    if (written < 0 || written >= PATH_MAX) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

static int owner_marker_matches(const struct profile *profile,
                                const char *global, const char *context)
{
    char path[PATH_MAX], actual[PATH_MAX + KUBE_CONTEXT_CAP + 64];
    if (owner_marker_path(profile, path, 0) != 0)
        return -1;
    int read = read_owned_regular_file(path, actual, sizeof(actual));
    if (read <= 0)
        return read;

    char expected[PATH_MAX + KUBE_CONTEXT_CAP + 64];
    int written = snprintf(expected, sizeof(expected),
                           "schema=1\npath=%s\ncontext=%s\n", global,
                           context);
    if (written < 0 || written >= (int)sizeof(expected)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return strcmp(actual, expected) == 0 ? 1 : 0;
}

static int write_owner_marker(const struct profile *profile, const char *global,
                              const char *context)
{
    char path[PATH_MAX], text[PATH_MAX + KUBE_CONTEXT_CAP + 64];
    if (owner_marker_path(profile, path, 1) != 0)
        return -1;
    int written = snprintf(text, sizeof(text),
                           "schema=1\npath=%s\ncontext=%s\n", global,
                           context);
    if (written < 0 || written >= (int)sizeof(text)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return fs_write_file_atomic(path, text, (size_t)written, 0600);
}

static int kubectl_view_json(const char *global, char *output, size_t cap)
{
    int truncated = 0;
    const char *command[] = {
        "kubectl", "--kubeconfig", global, "config", "view", "--raw",
        "-o", "json", NULL,
    };
    return proc_run_capture_checked(command, output, cap, &truncated) == 0 &&
        !truncated ? 0 : -1;
}

/* 1=found, 0=absent, -1=the kubectl JSON is not a kubeconfig record list. */
static int named_record(const cJSON *root, const char *section,
                        const char *name, const cJSON **out)
{
    *out = NULL;
    const cJSON *items = cJSON_GetObjectItemCaseSensitive(root, section);
    if (!items)
        return 0;
    if (!cJSON_IsArray(items))
        return -1;
    const cJSON *item;
    cJSON_ArrayForEach(item, items) {
        const cJSON *record_name = cJSON_GetObjectItemCaseSensitive(item,
                                                                      "name");
        if (!cJSON_IsObject(item) || !cJSON_IsString(record_name) ||
            !record_name->valuestring)
            return -1;
        if (strcmp(record_name->valuestring, name) == 0) {
            *out = item;
            return 1;
        }
    }
    return 0;
}

static int profile_records_are_owned(const cJSON *root, const char *name)
{
    const cJSON *context = NULL, *cluster = NULL, *user = NULL;
    int has_context = named_record(root, "contexts", name, &context);
    int has_cluster = named_record(root, "clusters", name, &cluster);
    int has_user = named_record(root, "users", name, &user);
    if (has_context < 0 || has_cluster < 0 || has_user < 0)
        return -1;
    if (has_context == 0 && has_cluster == 0 && has_user == 0)
        return 2;
    if (has_context != 1 || has_cluster != 1 || has_user != 1)
        return 0;

    const cJSON *context_data = cJSON_GetObjectItemCaseSensitive(context,
                                                                    "context");
    const cJSON *cluster_name = context_data ?
        cJSON_GetObjectItemCaseSensitive(context_data, "cluster") : NULL;
    const cJSON *user_name = context_data ?
        cJSON_GetObjectItemCaseSensitive(context_data, "user") : NULL;
    const cJSON *cluster_data = cJSON_GetObjectItemCaseSensitive(cluster,
                                                                    "cluster");
    const cJSON *server = cluster_data ?
        cJSON_GetObjectItemCaseSensitive(cluster_data, "server") : NULL;
    if (!cJSON_IsObject(context_data) || !cJSON_IsString(cluster_name) ||
        !cJSON_IsString(user_name) || !cJSON_IsObject(cluster_data) ||
        !cJSON_IsString(server) || !server->valuestring)
        return 0;
    return strcmp(cluster_name->valuestring, name) == 0 &&
        strcmp(user_name->valuestring, name) == 0 &&
        strncmp(server->valuestring, "https://127.0.0.1:",
                strlen("https://127.0.0.1:")) == 0;
}

static int reject_foreign_collision(const struct profile *profile,
                                    const char *global, const char *context,
                                    const char *json)
{
    cJSON *root = cJSON_Parse(json);
    if (!root || !cJSON_IsObject(root)) {
        cJSON_Delete(root);
        logerr("cannot validate existing kubeconfig JSON");
        return -1;
    }
    int owned = profile_records_are_owned(root, context);
    cJSON_Delete(root);
    if (owned < 0) {
        logerr("cannot validate existing kubeconfig records");
        return -1;
    }
    if (owned == 2)
        return 1;
    if (!owned)
        return 0;
    int marker = owner_marker_matches(profile, global, context);
    if (marker < 0) {
        logerr("cannot validate the Hamn kubeconfig ownership marker");
        return -1;
    }
    return marker == 1 ? 1 : 0;
}

/* A nonzero exit from current-context means the config has no active context.
 * view --raw has already established that kubectl itself can read the file. */
static int current_context(const char *global, char output[KUBE_CONTEXT_CAP])
{
    output[0] = '\0';
    const char *command[] = {
        "kubectl", "--kubeconfig", global, "config", "current-context", NULL,
    };
    if (proc_run_capture(command, output, KUBE_CONTEXT_CAP) != 0) {
        output[0] = '\0';
        return 0;
    }
    size_t length = strlen(output);
    if (length == 0 || length >= KUBE_CONTEXT_CAP ||
        strpbrk(output, "\r\n") != NULL) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

static int merge_profile_kubeconfig(const struct profile *profile,
                                    const char *global, char *output,
                                    size_t capacity)
{
    char local[PROFILE_PATH_CAP], env[PATH_MAX * 2 + 32];
    if (!profile_path(profile, "kubeconfig", local, sizeof(local)) ||
        strchr(local, ':') || strchr(global, ':')) {
        errno = EINVAL;
        return -1;
    }
    int written = snprintf(env, sizeof(env), "KUBECONFIG=%s:%s", local, global);
    if (written < 0 || written >= (int)sizeof(env)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    int truncated = 0;
    const char *command[] = {
        "/usr/bin/env", env, "kubectl", "config", "view", "--flatten",
        "--raw", "-o", "yaml", NULL,
    };
    if (proc_run_capture_checked(command, output, capacity, &truncated) != 0 ||
        truncated || !output[0])
        return -1;
    return 0;
}

static int temporary_kubeconfig(const char *global, char path[PATH_MAX])
{
    int written = snprintf(path, PATH_MAX, "%s", global);
    if (written < 0 || written >= PATH_MAX) {
        errno = ENAMETOOLONG;
        return -1;
    }
    char *slash = strrchr(path, '/');
    if (!slash || slash == path) {
        errno = EINVAL;
        return -1;
    }
    *slash = '\0';
    size_t length = strlen(path);
    static const char suffix[] = "/.hamn-kubeconfig.XXXXXX";
    if (length + sizeof(suffix) > PATH_MAX) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(path + length, suffix, sizeof(suffix));
    int fd = mkstemp(path);
    if (fd < 0)
        return -1;
    if (fchmod(fd, 0600) != 0) {
        int saved = errno;
        close(fd);
        unlink(path);
        errno = saved;
        return -1;
    }
    return fd;
}

static int write_all(int fd, const char *text, size_t length)
{
    while (length > 0) {
        ssize_t written = write(fd, text, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (written == 0) {
            errno = EIO;
            return -1;
        }
        text += written;
        length -= (size_t)written;
    }
    return 0;
}

static int select_context_in_text(const char *global, const char *input,
                                  const char *context, char *output,
                                  size_t capacity)
{
    char temporary[PATH_MAX];
    int fd = temporary_kubeconfig(global, temporary);
    if (fd < 0)
        return -1;
    int rc = -1;
    if (write_all(fd, input, strlen(input)) != 0 || fsync(fd) != 0)
        goto out;
    if (close(fd) != 0) {
        fd = -1;
        goto out;
    }
    fd = -1;
    const char *command[] = {
        "kubectl", "--kubeconfig", temporary, "config", "use-context",
        context, NULL,
    };
    if (proc_run(command) != 0)
        goto out;
    if (read_owned_regular_file(temporary, output, capacity) != 1 ||
        !output[0])
        goto out;
    rc = 0;
out:
    {
        int saved = errno;
        if (fd >= 0)
            close(fd);
        unlink(temporary);
        errno = saved;
    }
    return rc;
}

static int validate_global_config(const struct profile *profile,
                                  const char *global, const char *context)
{
    char json[KUBECONFIG_CAP];
    if (kubectl_view_json(global, json, sizeof(json)) != 0) {
        logerr("cannot read ~/.kube/config with kubectl; install a compatible "
               "kubectl client before enabling Kubernetes");
        return -1;
    }
    int ownership = reject_foreign_collision(profile, global, context, json);
    if (ownership < 0)
        return -1;
    if (ownership == 0) {
        logerr("kube context '%s' already belongs to foreign configuration; "
               "Hamn will not overwrite it", context);
        errno = EEXIST;
        return -1;
    }
    return 0;
}

static int prepare_global_config(const struct profile *profile,
                                 char global[PATH_MAX],
                                 char context[KUBE_CONTEXT_CAP], int *lock)
{
    *lock = -1;
    if (global_kubeconfig_path(global) != 0 ||
        context_name(profile, context) != 0 ||
        ensure_global_kubeconfig(global) != 0) {
        logerr("cannot prepare ~/.kube/config: %s", strerror(errno));
        return -1;
    }
    *lock = global_kubeconfig_lock(global);
    if (*lock < 0) {
        logerr("cannot lock ~/.kube/config: %s", strerror(errno));
        return -1;
    }
    if (owned_regular_file(global) != 0) {
        logerr("refusing unsafe ~/.kube/config");
        global_kubeconfig_unlock(*lock);
        *lock = -1;
        return -1;
    }
    if (validate_global_config(profile, global, context) != 0) {
        global_kubeconfig_unlock(*lock);
        *lock = -1;
        return -1;
    }
    return 0;
}

int kubeconfig_preflight_profile(const struct profile *profile)
{
    if (!profile) {
        errno = EINVAL;
        return -1;
    }
    char global[PATH_MAX], context[KUBE_CONTEXT_CAP];
    int lock;
    int rc = prepare_global_config(profile, global, context, &lock);
    global_kubeconfig_unlock(lock);
    return rc;
}

int kubeconfig_activate_profile_with_snapshot(
    const struct profile *profile, struct vm_state *state,
    struct kubeconfig_context_snapshot *snapshot)
{
    if (!profile || !state) {
        errno = EINVAL;
        return -1;
    }
    if (snapshot)
        memset(snapshot, 0, sizeof(*snapshot));
    char global[PATH_MAX], context[KUBE_CONTEXT_CAP];
    int lock;
    if (prepare_global_config(profile, global, context, &lock) != 0)
        return -1;

    int rc = -1;
    char previous[KUBE_CONTEXT_CAP], original[KUBECONFIG_CAP], merged[KUBECONFIG_CAP];
    if (current_context(global, previous) != 0) {
        logerr("cannot determine the current kube context: %s", strerror(errno));
        goto out;
    }
    if (read_owned_regular_file(global, original, sizeof(original)) != 1) {
        logerr("cannot snapshot ~/.kube/config before activation");
        goto out;
    }
    if (merge_profile_kubeconfig(profile, global, merged, sizeof(merged)) != 0 ||
        select_context_in_text(global, merged, context, merged,
                               sizeof(merged)) != 0) {
        logerr("cannot merge and select the Hamn K3s kubeconfig");
        goto out;
    }

    struct vm_state previous_state = *state;
    if (previous[0] && strcmp(previous, context) != 0 &&
        !state->prev_kube_context[0]) {
        snprintf(state->prev_kube_context, sizeof(state->prev_kube_context),
                 "%s", previous);
    }
    if (state_save(profile, state) != 0) {
        logerr("cannot persist the previous kube context: %s", strerror(errno));
        *state = previous_state;
        goto out;
    }
    if (fs_write_file_atomic(global, merged, strlen(merged), 0600) != 0) {
        logerr("cannot publish the Hamn K3s kubeconfig: %s", strerror(errno));
        *state = previous_state;
        (void)state_save(profile, state);
        goto out;
    }
    if (write_owner_marker(profile, global, context) != 0) {
        logerr("cannot persist kubeconfig ownership: %s", strerror(errno));
        if (fs_write_file_atomic(global, original, strlen(original), 0600) != 0)
            logerr("cannot restore ~/.kube/config after failed ownership save");
        *state = previous_state;
        (void)state_save(profile, state);
        goto out;
    }
    if (snapshot) {
        snapshot->present = previous[0] != '\0';
        snprintf(snapshot->name, sizeof(snapshot->name), "%s", previous);
    }
    logmsg("kube context '%s' is now active", context);
    rc = 0;
out:
    global_kubeconfig_unlock(lock);
    return rc;
}

int kubeconfig_activate_profile(const struct profile *profile,
                                struct vm_state *state)
{
    return kubeconfig_activate_profile_with_snapshot(profile, state, NULL);
}

static int clear_context_in_text(const char *global, const char *input,
                                 char *output, size_t capacity)
{
    char temporary[PATH_MAX];
    int fd = temporary_kubeconfig(global, temporary);
    if (fd < 0)
        return -1;
    int rc = -1;
    if (write_all(fd, input, strlen(input)) != 0 || fsync(fd) != 0)
        goto out;
    if (close(fd) != 0) {
        fd = -1;
        goto out;
    }
    fd = -1;
    const char *command[] = {
        "kubectl", "--kubeconfig", temporary, "config", "unset",
        "current-context", NULL,
    };
    if (proc_run(command) != 0 ||
        read_owned_regular_file(temporary, output, capacity) != 1 ||
        !output[0])
        goto out;
    rc = 0;
out:
    {
        int saved = errno;
        if (fd >= 0)
            close(fd);
        unlink(temporary);
        errno = saved;
    }
    return rc;
}

int kubeconfig_restore_context_snapshot(
    const struct profile *profile,
    const struct kubeconfig_context_snapshot *snapshot)
{
    if (!profile || !snapshot ||
        (snapshot->present && !snapshot->name[0])) {
        errno = EINVAL;
        return -1;
    }
    char global[PATH_MAX], context[KUBE_CONTEXT_CAP];
    if (global_kubeconfig_path(global) != 0 ||
        context_name(profile, context) != 0)
        return -1;
    if (owned_regular_file(global) != 0) {
        logerr("refusing unsafe ~/.kube/config while rolling back the kube context");
        return -1;
    }
    int lock = global_kubeconfig_lock(global);
    if (lock < 0) {
        logerr("cannot lock ~/.kube/config: %s", strerror(errno));
        return -1;
    }

    int rc = -1;
    char current[KUBE_CONTEXT_CAP];
    if (current_context(global, current) != 0) {
        logerr("cannot determine the current kube context: %s", strerror(errno));
        goto out;
    }
    if (strcmp(current, context) != 0) {
        rc = 0;
        goto out;
    }
    char original[KUBECONFIG_CAP], restored[KUBECONFIG_CAP];
    if (read_owned_regular_file(global, original, sizeof(original)) != 1 ||
        (snapshot->present ?
         select_context_in_text(global, original, snapshot->name, restored,
                                sizeof(restored)) :
         clear_context_in_text(global, original, restored,
                               sizeof(restored))) != 0 ||
        fs_write_file_atomic(global, restored, strlen(restored), 0600) != 0) {
        logerr("cannot restore the kube context after failed activation");
        goto out;
    }
    rc = 0;
out:
    global_kubeconfig_unlock(lock);
    return rc;
}

int kubeconfig_restore_previous(const struct profile *profile,
                                struct vm_state *state)
{
    if (!profile || !state || !state->prev_kube_context[0])
        return 0;
    char global[PATH_MAX], context[KUBE_CONTEXT_CAP];
    if (global_kubeconfig_path(global) != 0 || context_name(profile, context) != 0)
        return -1;
    if (lstat(global, &(struct stat){0}) != 0) {
        if (errno != ENOENT)
            return -1;
        state->prev_kube_context[0] = '\0';
        return state_save(profile, state);
    }
    if (owned_regular_file(global) != 0) {
        logerr("refusing unsafe ~/.kube/config while restoring the kube context");
        return -1;
    }
    int lock = global_kubeconfig_lock(global);
    if (lock < 0) {
        logerr("cannot lock ~/.kube/config: %s", strerror(errno));
        return -1;
    }
    int rc = -1;
    char current[KUBE_CONTEXT_CAP];
    if (current_context(global, current) != 0) {
        logerr("cannot determine the current kube context: %s", strerror(errno));
        goto out;
    }
    if (strcmp(current, context) == 0) {
        char original[KUBECONFIG_CAP], restored[KUBECONFIG_CAP];
        if (read_owned_regular_file(global, original, sizeof(original)) != 1 ||
            select_context_in_text(global, original, state->prev_kube_context,
                                   restored, sizeof(restored)) != 0 ||
            fs_write_file_atomic(global, restored, strlen(restored), 0600) != 0) {
            logerr("cannot restore kube context '%s'", state->prev_kube_context);
            goto out;
        }
        logmsg("kube context restored to '%s'", state->prev_kube_context);
    }
    state->prev_kube_context[0] = '\0';
    rc = state_save(profile, state);
out:
    global_kubeconfig_unlock(lock);
    return rc;
}
