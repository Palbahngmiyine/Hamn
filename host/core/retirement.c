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
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/state.h"
#include "sshmgr/ssh.h"
#include "../../build/generated/k3s_retirement.h"

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
