#include "core/retirement.h"
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "core/control.h"
#include "core/guest_deployment.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/state.h"
#include "sshmgr/ssh.h"
#include "util/fs.h"
#include "k3s_retirement.h"

int retirement_context(const struct profile *profile)
{
    char root[PROFILE_PATH_CAP], old_dir[PROFILE_PATH_CAP], new_dir[PROFILE_PATH_CAP];
    char source[PROFILE_PATH_CAP], target[PROFILE_PATH_CAP], context[128], expected[PROFILE_PATH_CAP + 256];
    if (!hamn_home(root, sizeof(root)) ||
        profile_docker_context_name(profile, context, sizeof(context)) != 0)
        return -1;
    if (snprintf(old_dir, sizeof(old_dir), "%s/.kube-contexts", root) >= (int)sizeof(old_dir) ||
        snprintf(new_dir, sizeof(new_dir), "%s/.retired-kube-contexts", root) >= (int)sizeof(new_dir) ||
        snprintf(source, sizeof(source), "%s/%s", old_dir, profile->name) >= (int)sizeof(source) ||
        snprintf(target, sizeof(target), "%s/%s", new_dir, profile->name) >= (int)sizeof(target))
        return -1;
    struct stat st;
    if (lstat(old_dir, &st) != 0) return errno == ENOENT ? 0 : -1;
    if (!S_ISDIR(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 0022)) return -1;
    int fd = open(source, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) return errno == ENOENT ? 0 : -1;
    char actual[sizeof(expected)];
    int valid = fstat(fd, &st) == 0 && S_ISREG(st.st_mode) && st.st_uid == geteuid() &&
        st.st_nlink == 1 && !(st.st_mode & 0022) && st.st_size > 0 && st.st_size < (off_t)sizeof(actual);
    ssize_t length = valid ? read(fd, actual, sizeof(actual) - 1) : -1;
    close(fd);
    if (length <= 0 || length != st.st_size) return -1;
    actual[length] = '\0';
    int size = snprintf(expected, sizeof(expected), "schema=1\npath=%s/.kube/config\ncontext=%s\n", getenv("HOME"), context);
    if (size < 0 || size >= (int)sizeof(expected) || length != size || memcmp(expected, actual, (size_t)size))
        return -1;
    if (mkdir(new_dir, 0700) != 0 && errno != EEXIST) return -1;
    if (lstat(new_dir, &st) != 0 || !S_ISDIR(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 0022))
        return -1;
    if (lstat(target, &st) == 0) {
        if (!S_ISREG(st.st_mode) || st.st_uid != geteuid() || st.st_nlink != 1) return -1;
    } else if (errno != ENOENT) return -1;
    if (fs_write_file_atomic(target, actual, (size_t)length, 0600) != 0) return -1;
    return unlink(source);
}

static int retire_forward(const struct profile *profile, const char *ip)
{
    char path[PROFILE_PATH_CAP], text[32];
    if (!profile_path(profile, "kube-api-port", path, sizeof(path))) return -1;
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) return errno == ENOENT ? 0 : -1;
    struct stat st;
    int valid = fstat(fd, &st) == 0 && S_ISREG(st.st_mode) &&
        st.st_uid == geteuid() && st.st_nlink == 1 && st.st_size > 0 && st.st_size < 32;
    ssize_t count = valid ? read(fd, text, sizeof(text) - 1) : -1;
    close(fd);
    if (count <= 0 || count != st.st_size) return -1;
    text[count] = '\0';
    char *end;
    unsigned long port = strtoul(text, &end, 10);
    if (end == text || (*end != '\0' && strcmp(end, "\n")) || port < 16443 || port >= 17467)
        return -1;
    if (ssh_forward_cancel_tcp(profile, ip, "127.0.0.1", (unsigned)port,
                                "127.0.0.1", 6443) != 0) {
        /* A retry after cancellation may find the listener already gone. */
        fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0) return -1;
        struct sockaddr_in address = { .sin_family = AF_INET,
            .sin_addr.s_addr = htonl(INADDR_LOOPBACK), .sin_port = htons((uint16_t)port) };
        int absent = bind(fd, (struct sockaddr *)&address, sizeof(address)) == 0;
        close(fd);
        if (!absent) return -1;
    }
    return unlink(path);
}

int retirement_recover(const struct profile *profile, const char *ip)
{
    const char *command[] = { "sudo", "python3", "-c", retirement_payload,
                              "recover-only", NULL };
    return ssh_exec(profile, ip, command, 0);
}

int retirement_run(struct profile *profile, const char *ip)
{
    if (!profile->legacy_k3s)
        return 0;
    if (!ip || !ip[0]) {
        logerr("K3s retirement requires a verified running VM address");
        return -1;
    }
    logmsg("retiring managed K3s for %s; Docker data is preserved", profile->name);
    const char *command[] = { "sudo", "python3", "-c", retirement_payload, NULL };
    if (ssh_exec(profile, ip, command, 0) != 0) {
        logerr("K3s retirement is incomplete; the next mutation will resume it");
        return -1;
    }
    if (retire_forward(profile, ip) != 0) {
        logerr("cannot safely remove the legacy Kubernetes API forward");
        return -1;
    }
    if (retirement_context(profile) != 0) {
        logerr("cannot safely retire the legacy Kubernetes context ownership record");
        return -1;
    }
    const char *files[] = { "kubeconfig", NULL };
    for (size_t i = 0; files[i]; i++) {
        char path[PROFILE_PATH_CAP];
        struct stat st;
        if (!profile_path(profile, files[i], path, sizeof(path)))
            return -1;
        if (lstat(path, &st) != 0) {
            if (errno == ENOENT) continue;
            return -1;
        }
        if (!S_ISREG(st.st_mode) || st.st_uid != geteuid() || st.st_nlink != 1 ||
            unlink(path) != 0) {
            logerr("cannot safely retire profile-local Kubernetes state: %s", path);
            return -1;
        }
    }
    /* Refresh the SSH session's supplementary groups and all socket forwards
     * before publishing completion. A listening host socket alone is not proof
     * that the guest permits Docker API access. */
    struct vm_state state;
    if (state_load(profile, &state) != 0)
        return -1;
    snprintf(state.ip, sizeof(state.ip), "%s", ip);
    ssh_master_exit(profile);
    if (ssh_master_start(profile, ip, 15) != 0 ||
        guest_deployment_repair_locked(profile, &state) != 0) {
        logerr("guest retirement finished but Docker reconnection failed; retry required");
        return -1;
    }
    profile->legacy_k3s = 0;
    profile->legacy_k3s_enabled = 0;
    if (profile_save(profile) != 0) {
        profile->legacy_k3s = 1;
        logerr("cannot publish K3s retirement; retry will resume guest completion");
        return -1;
    }
    return 0;
}

int hamn_control_migrate(const char *name)
{
    struct vm_lifecycle_lock lock;
    if (!profile_name_valid(name) || vm_lifecycle_lock_acquire(name, &lock) != 0)
        return 1;
    struct profile profile;
    struct vm_state state;
    int rc = 1, mutation = -1;
    if (profile_read_existing(&profile, name) != 0)
        goto out;
    if (!profile.legacy_k3s) { rc = 0; goto out; }
    mutation = profile_mutation_lock(&profile);
    if (mutation < 0) goto out;
    int live = vm_process_probe(&profile, NULL);
    if (live == VM_PROCESS_STALE) { rc = 0; goto out; }
    if (live != VM_PROCESS_VERIFIED || state_load(&profile, &state) != 0 || !state.ip[0]) {
        logerr("cannot verify VM ownership for K3s retirement");
        goto out;
    }
    if (ssh_master_start(&profile, state.ip, 15) == 0)
        rc = retirement_run(&profile, state.ip) == 0 ? 0 : 1;
out:
    if (mutation >= 0) profile_mutation_unlock(mutation);
    vm_lifecycle_lock_release(&lock);
    return rc;
}
