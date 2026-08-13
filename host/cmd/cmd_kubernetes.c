#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#include "cli.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/kubeconfig.h"
#include "core/mutation_lock.h"
#include "core/profile.h"
#include "core/state.h"
#include "fwd/ports.h"
#include "sshmgr/ssh.h"
#include "util/fs.h"

#define KUBE_PORT_FIRST 16443U
#define KUBE_PORT_COUNT 1024U
#define KUBECONFIG_CAP (256U * 1024U)
#define PROFILE_CONFIG_FILE "config.yaml"

struct kube_port_transaction {
    int previous_present;
    unsigned previous_port;
    int previous_forward_touched;
    int replacement_forwarded;
    unsigned replacement_port;
    int port_file_touched;
};

struct profile_file_snapshot {
    char *text;
    size_t length;
    int present;
};

struct k3s_lifecycle_state {
    int active;
    int enabled;
};

struct kubernetes_start_transaction {
    struct kube_port_transaction port;
    struct profile_file_snapshot kubeconfig_file;
    struct profile_file_snapshot config_file;
    struct kubeconfig_context_snapshot context;
    struct k3s_lifecycle_state guest_state;
    struct vm_state state_before;
    int guest_started;
    int context_activated;
    int profile_save_attempted;
    int kubernetes_enabled_before;
};

static void kubernetes_usage(FILE *stream)
{
    fprintf(stream,
            "usage: hamn kubernetes [-p PROFILE] <start|stop|status|delete> "
            "[PROFILE]\n");
}

static unsigned profile_port_seed(const char *name)
{
    if (strcmp(name, "default") == 0)
        return 0;
    unsigned hash = 2166136261u;
    for (const unsigned char *cursor = (const unsigned char *)name; *cursor;
         cursor++)
        hash = (hash ^ *cursor) * 16777619u;
    return hash % KUBE_PORT_COUNT;
}

static int kube_port_available(unsigned port)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0)
        return 0;
    int reuse = 1;
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_port = htons((in_port_t)port),
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK),
    };
    int available = setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse,
                               sizeof(reuse)) == 0 &&
                    bind(fd, (const struct sockaddr *)&address,
                         sizeof(address)) == 0;
    close(fd);
    return available;
}

static int kube_port_read(const struct profile *profile, unsigned *port)
{
    char path[PROFILE_PATH_CAP], text[32];
    if (!profile_path(profile, "kube-api-port", path, sizeof(path)))
        return -1;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return errno == ENOENT ? 0 : -1;
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_size <= 0 || status.st_size >= (off_t)sizeof(text)) {
        int saved = errno ? errno : EINVAL;
        close(fd);
        errno = saved;
        return -1;
    }
    ssize_t count = read(fd, text, sizeof(text) - 1);
    int saved = errno;
    if (close(fd) != 0 && count >= 0)
        return -1;
    if (count <= 0) {
        errno = count == 0 ? EINVAL : saved;
        return -1;
    }
    text[count] = '\0';
    char *end = NULL;
    errno = 0;
    unsigned long value = strtoul(text, &end, 10);
    if (errno || end == text || (*end == '\n' && *++end != '\0') ||
        *end != '\0' || value < KUBE_PORT_FIRST ||
        value >= KUBE_PORT_FIRST + KUBE_PORT_COUNT) {
        errno = EINVAL;
        return -1;
    }
    *port = (unsigned)value;
    return 1;
}

static int kube_port_write(const struct profile *profile, unsigned port)
{
    char path[PROFILE_PATH_CAP], text[32];
    if (!profile_path(profile, "kube-api-port", path, sizeof(path))) {
        errno = ENAMETOOLONG;
        return -1;
    }
    int length = snprintf(text, sizeof(text), "%u\n", port);
    if (length <= 0 || length >= (int)sizeof(text)) {
        errno = EOVERFLOW;
        return -1;
    }
    return fs_write_file_atomic(path, text, (size_t)length, 0600);
}

static int kube_port_remove_record(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, "kube-api-port", path, sizeof(path))) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return fs_unlink_if_exists(path);
}

static unsigned kube_port_candidate(const struct profile *profile,
                                    unsigned attempt)
{
    return KUBE_PORT_FIRST +
        (profile_port_seed(profile->name) + attempt) % KUBE_PORT_COUNT;
}

static int kube_port_cancel(const struct profile *profile, const char *ip,
                            unsigned port)
{
    return ssh_forward_cancel_tcp(profile, ip, "127.0.0.1", port,
                                  "127.0.0.1", 6443) == 0 ? 0 : -1;
}

static int kube_port_transaction_begin(const struct profile *profile,
                                       struct kube_port_transaction *transaction)
{
    memset(transaction, 0, sizeof(*transaction));
    int stored = kube_port_read(profile, &transaction->previous_port);
    if (stored < 0) {
        logerr("cannot read Kubernetes API port: %s", strerror(errno));
        return -1;
    }
    transaction->previous_present = stored == 1;
    return 0;
}

static int kube_port_forward(const struct profile *profile, const char *ip,
                             struct kube_port_transaction *transaction,
                             unsigned *port_out)
{
    if (transaction->previous_present) {
        if (kube_port_cancel(profile, ip, transaction->previous_port) != 0) {
            logerr("cannot replace Kubernetes API forwarding");
            return -1;
        }
        transaction->previous_forward_touched = 1;
    }

    for (unsigned attempt = 0; attempt < KUBE_PORT_COUNT; attempt++) {
        unsigned port = transaction->previous_present && attempt == 0 ?
            transaction->previous_port : kube_port_candidate(profile, attempt);
        if (!kube_port_available(port))
            continue;
        if (ssh_forward_add_tcp(profile, ip, "127.0.0.1", port,
                                "127.0.0.1", 6443) != 0)
            continue;
        transaction->replacement_forwarded = 1;
        transaction->replacement_port = port;
        transaction->port_file_touched = 1;
        if (kube_port_write(profile, port) != 0) {
            logerr("cannot persist Kubernetes API port: %s", strerror(errno));
            return -1;
        }
        *port_out = port;
        return 0;
    }
    logerr("no free loopback port is available for the Kubernetes API");
    return -1;
}

static int kube_port_transaction_restore(
    const struct profile *profile, const char *ip,
    const struct kube_port_transaction *transaction)
{
    int failed = 0;
    if (transaction->replacement_forwarded &&
        kube_port_cancel(profile, ip, transaction->replacement_port) != 0) {
        logerr("cannot remove replacement Kubernetes API forwarding");
        failed = 1;
    }
    if (transaction->previous_forward_touched &&
        ssh_forward_add_tcp(profile, ip, "127.0.0.1",
                            transaction->previous_port, "127.0.0.1", 6443) != 0) {
        logerr("cannot restore previous Kubernetes API forwarding");
        failed = 1;
    }
    if (transaction->port_file_touched) {
        int restored = transaction->previous_present ?
            kube_port_write(profile, transaction->previous_port) :
            kube_port_remove_record(profile);
        if (restored != 0) {
            logerr("cannot restore Kubernetes API port record: %s",
                   strerror(errno));
            failed = 1;
        }
    }
    return failed ? -1 : 0;
}

static int text_append(char *output, size_t capacity, size_t *offset,
                       const char *line)
{
    int written = snprintf(output + *offset, capacity - *offset, "%s\n", line);
    if (written < 0 || written >= (int)(capacity - *offset))
        return -1;
    *offset += (size_t)written;
    return 0;
}

static int rewrite_kubeconfig(char *input, char *output, size_t capacity,
                              const char *context, unsigned port)
{
    size_t offset = 0;
    unsigned server_count = 0, name_count = 0, cluster_count = 0;
    unsigned user_count = 0, current_context_count = 0;
    char *save = NULL;
    for (char *line = strtok_r(input, "\n", &save); line;
         line = strtok_r(NULL, "\n", &save)) {
        const char *trimmed = line;
        while (*trimmed == ' ')
            trimmed++;
        size_t indent = (size_t)(trimmed - line);
        char replacement[512];
        if (strncmp(trimmed, "server:", 7) == 0) {
            int written = snprintf(replacement, sizeof(replacement),
                                   "%*sserver: https://127.0.0.1:%u",
                                   (int)indent, "", port);
            if (written < 0 || written >= (int)sizeof(replacement))
                return -1;
            line = replacement;
            server_count++;
        } else if (strcmp(trimmed, "name: default") == 0 ||
                   strcmp(trimmed, "- name: default") == 0 ||
                   strcmp(trimmed, "cluster: default") == 0 ||
                   strcmp(trimmed, "user: default") == 0 ||
                   strcmp(trimmed, "current-context: default") == 0) {
            const char *colon = strchr(trimmed, ':');
            int written = snprintf(replacement, sizeof(replacement),
                                   "%*s%.*s: %s", (int)indent, "",
                                   (int)(colon - trimmed), trimmed, context);
            if (written < 0 || written >= (int)sizeof(replacement))
                return -1;
            line = replacement;
            if (strncmp(trimmed, "cluster:", 8) == 0)
                cluster_count++;
            else if (strncmp(trimmed, "user:", 5) == 0)
                user_count++;
            else if (strncmp(trimmed, "current-context:", 16) == 0)
                current_context_count++;
            else
                name_count++;
        }
        if (text_append(output, capacity, &offset, line) != 0)
            return -1;
    }
    return server_count == 1 && name_count == 3 && cluster_count == 1 &&
           user_count == 1 && current_context_count == 1 ? 0 : -1;
}

static int sync_kubeconfig(const struct profile *profile, const char *ip,
                           unsigned port)
{
    char input[KUBECONFIG_CAP], output[KUBECONFIG_CAP], context[128];
    int truncated = 0;
    const char *remote[] = {
        "sudo", "cat", "/etc/rancher/k3s/k3s.yaml", NULL
    };
    if (profile_docker_context_name(profile, context, sizeof(context)) != 0 ||
        ssh_exec_capture_checked(profile, ip, remote, input, sizeof(input),
                                 &truncated) != 0 || truncated ||
        rewrite_kubeconfig(input, output, sizeof(output), context, port) != 0) {
        logerr("cannot read or validate the K3s kubeconfig");
        return -1;
    }
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, "kubeconfig", path, sizeof(path)) ||
        fs_write_file_atomic(path, output, strlen(output), 0600) != 0) {
        logerr("cannot write kubeconfig: %s", strerror(errno));
        return -1;
    }
    return 0;
}

static int remove_kubeconfig(const struct profile *profile)
{
    char path[PROFILE_PATH_CAP];
    if (!profile_path(profile, "kubeconfig", path, sizeof(path)) ||
        fs_unlink_if_exists(path) != 0) {
        logerr("cannot remove kubeconfig: %s", strerror(errno));
        return -1;
    }
    return 0;
}

static void profile_file_snapshot_dispose(struct profile_file_snapshot *snapshot)
{
    free(snapshot->text);
    memset(snapshot, 0, sizeof(*snapshot));
}

static int profile_file_snapshot_capture(
    const struct profile *profile, const char *file,
    struct profile_file_snapshot *snapshot)
{
    memset(snapshot, 0, sizeof(*snapshot));
    char path[PROFILE_PATH_CAP];
    if (!file || !file[0] ||
        !profile_path(profile, file, path, sizeof(path))) {
        errno = ENAMETOOLONG;
        return -1;
    }
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return errno == ENOENT ? 0 : -1;
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_size < 0 || status.st_size >= (off_t)KUBECONFIG_CAP) {
        int saved = errno ? errno : EINVAL;
        close(fd);
        errno = saved;
        return -1;
    }
    snapshot->length = (size_t)status.st_size;
    snapshot->text = malloc(snapshot->length + 1);
    if (!snapshot->text) {
        close(fd);
        return -1;
    }
    size_t offset = 0;
    while (offset < snapshot->length) {
        ssize_t count = read(fd, snapshot->text + offset,
                             snapshot->length - offset);
        if (count < 0 && errno == EINTR)
            continue;
        if (count <= 0) {
            int saved = count == 0 ? EIO : errno;
            close(fd);
            profile_file_snapshot_dispose(snapshot);
            errno = saved;
            return -1;
        }
        offset += (size_t)count;
    }
    if (close(fd) != 0) {
        profile_file_snapshot_dispose(snapshot);
        return -1;
    }
    snapshot->text[snapshot->length] = '\0';
    snapshot->present = 1;
    return 0;
}

static int profile_file_snapshot_restore(
    const struct profile *profile, const char *file,
    const struct profile_file_snapshot *snapshot)
{
    char path[PROFILE_PATH_CAP];
    if (!file || !file[0] ||
        !profile_path(profile, file, path, sizeof(path))) {
        errno = ENAMETOOLONG;
        return -1;
    }
    if (!snapshot->present)
        return fs_unlink_if_exists(path);
    return fs_write_file_atomic(path, snapshot->text, snapshot->length, 0600);
}

static int load_running_profile(struct profile *profile, struct vm_state *state,
                                const char *name)
{
    if (profile_load(profile, name) != 0) {
        logerr("cannot load profile");
        return -1;
    }
    if (vm_running_pid(profile) < 0) {
        logerr("profile %s is not running (run: hamn start --profile %s)",
               profile->name, profile->name);
        return -1;
    }
    if (state_load(profile, state) != 0 || !state->ip[0]) {
        logerr("guest IP is unknown; restart with: hamn start");
        return -1;
    }
    return 0;
}

static int guest_k3s_command(const struct profile *profile, const char *ip,
                             const char *action)
{
    const char *command[] = {
        "sudo", "/usr/local/libexec/hamn/configure-k3s", action, NULL
    };
    return ssh_exec(profile, ip, command, 0) == 0 ? 0 : -1;
}

static int guest_k3s_lifecycle_state(const struct profile *profile,
                                     const char *ip,
                                     struct k3s_lifecycle_state *state)
{
    char output[64], expected[64];
    int truncated = 0;
    const char *command[] = {
        "sudo", "/usr/local/libexec/hamn/configure-k3s", "lifecycle-state",
        NULL,
    };
    if (ssh_exec_capture_checked(profile, ip, command, output, sizeof(output),
                                 &truncated) != 0 || truncated ||
        sscanf(output, "active=%d enabled=%d", &state->active,
               &state->enabled) != 2 ||
        (state->active != 0 && state->active != 1) ||
        (state->enabled != 0 && state->enabled != 1) ||
        snprintf(expected, sizeof(expected), "active=%d enabled=%d",
                 state->active, state->enabled) >= (int)sizeof(expected) ||
        strcmp(output, expected) != 0) {
        logerr("cannot determine the current K3s service state");
        return -1;
    }
    return 0;
}

static int guest_k3s_restore_state(const struct profile *profile,
                                   const char *ip,
                                   const struct k3s_lifecycle_state *state)
{
    const char *command[] = {
        "sudo", "/usr/local/libexec/hamn/configure-k3s", "restore-state",
        state->active ? "1" : "0", state->enabled ? "1" : "0", NULL,
    };
    return ssh_exec(profile, ip, command, 0) == 0 ? 0 : -1;
}

static void kubernetes_start_rollback(
    struct profile *profile, struct vm_state *state,
    struct kubernetes_start_transaction *transaction)
{
    int failed = 0;
    if (transaction->guest_started &&
        guest_k3s_restore_state(profile, state->ip,
                                &transaction->guest_state) != 0) {
        logerr("cannot restore the previous K3s service state");
        failed = 1;
    }
    if (kube_port_transaction_restore(profile, state->ip,
                                      &transaction->port) != 0)
        failed = 1;
    if (profile_file_snapshot_restore(profile, "kubeconfig",
                                      &transaction->kubeconfig_file) != 0) {
        logerr("cannot restore the profile kubeconfig: %s", strerror(errno));
        failed = 1;
    }
    if (transaction->context_activated &&
        kubeconfig_restore_context_snapshot(profile,
                                            &transaction->context) != 0)
        failed = 1;
    *state = transaction->state_before;
    if (state_save(profile, state) != 0) {
        logerr("cannot restore the previous kube context state: %s",
               strerror(errno));
        failed = 1;
    }
    profile->kubernetes_enabled = transaction->kubernetes_enabled_before;
    if (transaction->profile_save_attempted &&
        profile_file_snapshot_restore(profile, PROFILE_CONFIG_FILE,
                                      &transaction->config_file) != 0) {
        logerr("cannot restore the profile configuration: %s", strerror(errno));
        failed = 1;
    }
    if (failed)
        logerr("Kubernetes start rollback is incomplete");
}

static int kubernetes_start(struct profile *profile, struct vm_state *state)
{
    int mutation = profile_mutation_lock(profile);
    if (mutation < 0) {
        logerr("another %s profile mutation is running", profile->name);
        return 1;
    }
    int ports = port_forward_operation_lock(profile);
    if (ports < 0) {
        logerr("cannot lock host port-forward operations");
        profile_mutation_unlock(mutation);
        return 1;
    }
    int rc = 1;
    struct kubernetes_start_transaction transaction;
    memset(&transaction, 0, sizeof(transaction));
    transaction.state_before = *state;
    transaction.kubernetes_enabled_before = profile->kubernetes_enabled;
    if (kubeconfig_preflight_profile(profile) != 0)
        goto cleanup;
    if (guest_k3s_lifecycle_state(profile, state->ip,
                                  &transaction.guest_state) != 0)
        goto cleanup;
    if (kube_port_transaction_begin(profile, &transaction.port) != 0)
        goto cleanup;
    if (profile_file_snapshot_capture(profile, PROFILE_CONFIG_FILE,
                                      &transaction.config_file) != 0) {
        logerr("cannot snapshot the profile configuration: %s", strerror(errno));
        goto cleanup;
    }
    if (profile_file_snapshot_capture(profile, "kubeconfig",
                                      &transaction.kubeconfig_file) != 0) {
        logerr("cannot snapshot the profile kubeconfig: %s", strerror(errno));
        goto cleanup;
    }
    if (guest_k3s_command(profile, state->ip, "start") != 0) {
        logerr("cannot start K3s with system containerd CRI");
        goto cleanup;
    }
    transaction.guest_started = 1;
    unsigned port;
    if (kube_port_forward(profile, state->ip, &transaction.port, &port) != 0)
        goto rollback;
    if (sync_kubeconfig(profile, state->ip, port) != 0)
        goto rollback;
    if (kubeconfig_activate_profile_with_snapshot(profile, state,
                                                   &transaction.context) != 0)
        goto rollback;
    transaction.context_activated = 1;
    profile->kubernetes_enabled = 1;
    transaction.profile_save_attempted = 1;
    if (profile_save(profile) != 0) {
        logerr("cannot persist Kubernetes configuration: %s", strerror(errno));
        goto rollback;
    }
    logmsg("Kubernetes is ready for profile %s; use: hamn kubectl get nodes",
           profile->name);
    rc = 0;
    goto cleanup;
rollback:
    kubernetes_start_rollback(profile, state, &transaction);
cleanup:
    profile_file_snapshot_dispose(&transaction.kubeconfig_file);
    profile_file_snapshot_dispose(&transaction.config_file);
    port_forward_operation_unlock(ports);
    profile_mutation_unlock(mutation);
    return rc;
}

static int kubernetes_stop_or_delete(struct profile *profile,
                                     struct vm_state *state,
                                     int deleting)
{
    int mutation = profile_mutation_lock(profile);
    if (mutation < 0) {
        logerr("another %s profile mutation is running", profile->name);
        return 1;
    }
    int ports = port_forward_operation_lock(profile);
    if (ports < 0) {
        logerr("cannot lock host port-forward operations");
        profile_mutation_unlock(mutation);
        return 1;
    }
    int failed = guest_k3s_command(profile, state->ip,
                                   deleting ? "delete" : "stop") != 0;
    unsigned port;
    int stored = kube_port_read(profile, &port);
    if (stored < 0) {
        logerr("cannot read Kubernetes API port: %s", strerror(errno));
        failed = 1;
    } else if (stored == 1 && kube_port_cancel(profile, state->ip, port) != 0) {
        logerr("cannot stop Kubernetes API forwarding");
        failed = 1;
    }
    if (remove_kubeconfig(profile) != 0)
        failed = 1;
    if (kubeconfig_restore_previous(profile, state) != 0)
        failed = 1;
    if (deleting && !failed) {
        profile->kubernetes_enabled = 0;
        if (profile_save(profile) != 0) {
            logerr("cannot persist Kubernetes configuration: %s", strerror(errno));
            failed = 1;
        }
    }
    port_forward_operation_unlock(ports);
    profile_mutation_unlock(mutation);
    if (!failed)
        logmsg(deleting ? "deleted Kubernetes state for profile %s" :
               "stopped Kubernetes for profile %s", profile->name);
    return failed ? 1 : 0;
}

int cmd_kubernetes(int argc, char **argv)
{
    const char *action = NULL;
    const char *flag_profile = NULL;
    const char *positional_profile = NULL;
    for (int index = 1; index < argc; index++) {
        const char *argument = argv[index];
        if (strcmp(argument, "--profile") == 0 || strcmp(argument, "-p") == 0) {
            if (++index >= argc || flag_profile) {
                kubernetes_usage(stderr);
                return 2;
            }
            flag_profile = argv[index];
        } else if (strncmp(argument, "--profile=", 10) == 0) {
            if (flag_profile || !argument[10]) {
                kubernetes_usage(stderr);
                return 2;
            }
            flag_profile = argument + 10;
        } else if (strcmp(argument, "--help") == 0 || strcmp(argument, "-h") == 0) {
            kubernetes_usage(stdout);
            return 0;
        } else if (!action) {
            action = argument;
        } else if (!positional_profile) {
            positional_profile = argument;
        } else {
            kubernetes_usage(stderr);
            return 2;
        }
    }
    if (!action || (strcmp(action, "start") != 0 &&
                    strcmp(action, "stop") != 0 &&
                    strcmp(action, "status") != 0 &&
                    strcmp(action, "delete") != 0)) {
        kubernetes_usage(stderr);
        return 2;
    }

    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(flag_profile, positional_profile, profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }
    int mutation = strcmp(action, "start") == 0 ||
                   strcmp(action, "stop") == 0 ||
                   strcmp(action, "delete") == 0;
    struct vm_lifecycle_lock lifecycle = { .fd = -1 };
    if (mutation && vm_lifecycle_lock_acquire(profile_name, &lifecycle) != 0) {
        logerr("cannot lock the %s profile lifecycle", profile_name);
        return 1;
    }

    struct profile profile;
    struct vm_state state;
    int rc;
    if (strcmp(action, "status") == 0) {
        if (profile_load(&profile, profile_name) != 0) {
            logerr("cannot load profile");
            rc = 1;
        } else if (!profile.kubernetes_enabled) {
            printf("disabled\n");
            rc = 0;
        } else if (vm_running_pid(&profile) < 0 || state_load(&profile, &state) != 0 ||
                   !state.ip[0]) {
            printf("stopped\n");
            rc = 0;
        } else {
            rc = guest_k3s_command(&profile, state.ip, "status") == 0 ? 0 : 1;
        }
    } else if (load_running_profile(&profile, &state, profile_name) != 0) {
        rc = 1;
    } else if (strcmp(action, "start") == 0) {
        rc = kubernetes_start(&profile, &state);
    } else {
        rc = kubernetes_stop_or_delete(&profile, &state,
                                       strcmp(action, "delete") == 0);
    }
    vm_lifecycle_lock_release(&lifecycle);
    return rc;
}

#ifdef HAMN_TEST
int hamn_test_kubernetes_start(struct profile *profile, struct vm_state *state)
{
    return kubernetes_start(profile, state);
}
#endif
