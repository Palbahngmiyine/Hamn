#include "core/lifecycle.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <libproc.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/log.h"
#include "core/kubeconfig.h"
#include "core/state.h"
#include "fwd/docker_observer.h"
#include "fwd/mount_inotify.h"
#include "fwd/ports.h"
#include "sshmgr/ssh.h"
#include "util/fs.h"
#include "util/proc.h"
#include "vmrun/ctlsock.h"

int vm_lifecycle_lock_acquire(const char *profile_name,
                              struct vm_lifecycle_lock *lock)
{
    if (!lock)
        return -1;
    lock->fd = -1;
    if (!profile_name || !profile_name[0] || strchr(profile_name, '/') ||
        strcmp(profile_name, ".") == 0 || strcmp(profile_name, "..") == 0) {
        errno = EINVAL;
        return -1;
    }

    char root[1024], lock_dir[1100], path[1200];
    if (!hamn_home(root, sizeof(root))) {
        errno = EINVAL;
        return -1;
    }
    int n = snprintf(lock_dir, sizeof(lock_dir), "%s/.locks", root);
    if (n < 0 || n >= (int)sizeof(lock_dir) ||
        fs_mkdirs(lock_dir, 0700) != 0)
        return -1;
    n = snprintf(path, sizeof(path), "%s/%s.lock", lock_dir, profile_name);
    if (n < 0 || n >= (int)sizeof(path)) {
        errno = ENAMETOOLONG;
        return -1;
    }

    int fd = open(path, O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (fd < 0)
        return -1;
    struct stat sb;
    if (fstat(fd, &sb) != 0 || !S_ISREG(sb.st_mode)) {
        close(fd);
        errno = EINVAL;
        return -1;
    }
    while (flock(fd, LOCK_EX) != 0) {
        if (errno == EINTR)
            continue;
        close(fd);
        return -1;
    }
    lock->fd = fd;
    return 0;
}

void vm_lifecycle_lock_release(struct vm_lifecycle_lock *lock)
{
    if (!lock || lock->fd < 0)
        return;
    /*
     * flock ownership follows the inherited open file description. Closing
     * this process's descriptor must not unlock a supervisor child that still
     * owns the same lifecycle operation.
     */
    close(lock->fd);
    lock->fd = -1;
}

enum spawn_guard_state {
    SPAWN_GUARD_ABSENT = 0,
    SPAWN_GUARD_STALE,
    SPAWN_GUARD_LOCKED,
    SPAWN_GUARD_UNCERTAIN,
};

static int write_all(int fd, const char *data, size_t len)
{
    while (len > 0) {
        ssize_t n = write(fd, data, len);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (n == 0) {
            errno = EIO;
            return -1;
        }
        data += n;
        len -= (size_t)n;
    }
    return 0;
}

static int profile_dir_sync(const struct profile *p)
{
    int fd = open(p->dir, O_RDONLY | O_DIRECTORY);
    if (fd < 0)
        return -1;
    int rc = fsync(fd);
    int saved = errno;
    close(fd);
    errno = saved;
    return rc;
}

static int same_open_path_inode(int fd, const char *path)
{
    struct stat open_sb, path_sb;
    return fstat(fd, &open_sb) == 0 && S_ISREG(open_sb.st_mode) &&
           lstat(path, &path_sb) == 0 && S_ISREG(path_sb.st_mode) &&
           open_sb.st_dev == path_sb.st_dev &&
           open_sb.st_ino == path_sb.st_ino;
}

static enum spawn_guard_state spawn_guard_probe(const struct profile *p)
{
    char path[1024];
    profile_path(p, "vmrun.spawning", path, sizeof(path));
    int fd = open(path, O_RDWR | O_NOFOLLOW);
    if (fd < 0)
        return errno == ENOENT ? SPAWN_GUARD_ABSENT :
                                SPAWN_GUARD_UNCERTAIN;
    struct stat sb;
    if (fstat(fd, &sb) != 0 || !S_ISREG(sb.st_mode) ||
        sb.st_uid != geteuid() || (sb.st_mode & 077) != 0 ||
        sb.st_nlink != 1 || !same_open_path_inode(fd, path)) {
        close(fd);
        return SPAWN_GUARD_UNCERTAIN;
    }
    if (flock(fd, LOCK_EX | LOCK_NB) == 0) {
        (void)flock(fd, LOCK_UN);
        close(fd);
        return SPAWN_GUARD_STALE;
    }
    int saved = errno;
    close(fd);
    return saved == EWOULDBLOCK || saved == EAGAIN ?
           SPAWN_GUARD_LOCKED : SPAWN_GUARD_UNCERTAIN;
}

int vm_spawn_guard_create(const struct profile *p, int *fd_out,
                          char token[VM_SPAWN_TOKEN_HEX_SIZE])
{
    if (!p || !fd_out || !token) {
        errno = EINVAL;
        return -1;
    }
    *fd_out = -1;

    unsigned char random[16];
    arc4random_buf(random, sizeof(random));
    static const char digits[] = "0123456789abcdef";
    for (int i = 0; i < 16; i++) {
        token[i * 2] = digits[random[i] >> 4];
        token[i * 2 + 1] = digits[random[i] & 0x0f];
    }
    token[32] = '\0';

    char path[1024];
    profile_path(p, "vmrun.spawning", path, sizeof(path));
    int fd = open(path, O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (fd < 0)
        return -1;
    int rc = -1;
    if (flock(fd, LOCK_EX | LOCK_NB) != 0)
        goto out;
    char text[VM_SPAWN_TOKEN_HEX_SIZE + 1];
    int n = snprintf(text, sizeof(text), "%s\n", token);
    if (n != VM_SPAWN_TOKEN_HEX_SIZE ||
        write_all(fd, text, (size_t)n) != 0 || fsync(fd) != 0 ||
        profile_dir_sync(p) != 0)
        goto out;
    *fd_out = fd;
    return 0;

out:
    {
        int saved = errno;
        if (same_open_path_inode(fd, path))
            (void)unlink(path);
        close(fd);
        errno = saved;
    }
    return rc;
}

int vm_spawn_guard_release(const struct profile *p, int fd, int remove)
{
    if (!p || fd < 0) {
        errno = EINVAL;
        return -1;
    }
    int rc = 0;
    if (remove) {
        char path[1024];
        profile_path(p, "vmrun.spawning", path, sizeof(path));
        if (!same_open_path_inode(fd, path) || unlink(path) != 0 ||
            profile_dir_sync(p) != 0)
            rc = -1;
    }
    if (close(fd) != 0)
        rc = -1;
    return rc;
}

int vm_process_wait_spawn_transition(const struct profile *p,
                                     int timeout_dsec)
{
    if (!p || timeout_dsec < 1)
        return -1;
    for (int i = 0; i < timeout_dsec; i++) {
        enum spawn_guard_state state = spawn_guard_probe(p);
        if (state == SPAWN_GUARD_ABSENT || state == SPAWN_GUARD_STALE)
            return 0;
        if (state == SPAWN_GUARD_UNCERTAIN)
            return -1;
        usleep(100 * 1000);
    }
    return -1;
}

/* Hamn이 이 profile context를 활성화한 경우에만 이전 context로 복원한다. */
static int restore_docker_context(const struct profile *profile,
                                  struct vm_state *st)
{
    if (!st->prev_docker_context[0])
        return 0;

    char expected[128];
    if (profile_docker_context_name(profile, expected, sizeof(expected)) != 0)
        return -1;

    char cur[128] = "";
    const char *show[] = { "docker", "context", "show", NULL };
    if (proc_run_capture(show, cur, sizeof(cur)) == 0 &&
        strcmp(cur, expected) == 0) {
        char out[256] = "";
        const char *use[] = { "docker", "context", "use",
                              st->prev_docker_context, NULL };
        if (proc_run_capture(use, out, sizeof(out)) == 0) {
            logmsg("docker context restored to '%s'",
                   st->prev_docker_context);
        } else {
            logerr("docker context restore failed: %s",
                   out[0] ? out : "no output");
            return -1;
        }
    }
    st->prev_docker_context[0] = '\0';
    return 0;
}

struct process_identity {
    int pid;
    uint64_t start_sec;
    uint64_t start_usec;
    int has_executable_identity;
    unsigned char executable_uuid[16];
};

static int hex_value(char ch)
{
    if (ch >= '0' && ch <= '9')
        return ch - '0';
    if (ch >= 'a' && ch <= 'f')
        return ch - 'a' + 10;
    return -1;
}

static int executable_uuid_parse(const char *hex, unsigned char uuid[16])
{
    if (strlen(hex) != 32)
        return -1;
    for (int i = 0; i < 16; i++) {
        int high = hex_value(hex[i * 2]);
        int low = hex_value(hex[i * 2 + 1]);
        if (high < 0 || low < 0)
            return -1;
        uuid[i] = (unsigned char)((high << 4) | low);
    }
    return 0;
}

static int regular_file_read(const char *path, char *buf, size_t cap)
{
    struct stat sb;
    if (lstat(path, &sb) != 0 || !S_ISREG(sb.st_mode))
        return -1;
    FILE *f = fopen(path, "r");
    if (!f)
        return -1;
    size_t n = fread(buf, 1, cap - 1, f);
    int failed = ferror(f) || (n == cap - 1 && !feof(f));
    fclose(f);
    if (failed)
        return -1;
    buf[n] = '\0';
    return 0;
}

static int pid_file_read(const struct profile *p, int *pid)
{
    char path[1024], buf[64], extra;
    profile_path(p, "vmrun.pid", path, sizeof(path));
    if (regular_file_read(path, buf, sizeof(buf)) != 0)
        return -1;
    long value = 0;
    if (sscanf(buf, "%ld %c", &value, &extra) != 1 || value <= 1 ||
        value > INT_MAX)
        return -1;
    *pid = (int)value;
    return 0;
}

static int pid_file_write(const struct profile *p, int pid)
{
    char path[1024], text[64];
    profile_path(p, "vmrun.pid", path, sizeof(path));
    int n = snprintf(text, sizeof(text), "%d\n", pid);
    if (n <= 0 || n >= (int)sizeof(text))
        return -1;
    return fs_write_file_atomic(path, text, (size_t)n, 0600);
}

static int stored_identity_read(const struct profile *p,
                                struct process_identity *identity)
{
    char path[1024], buf[160], extra;
    profile_path(p, "vmrun.identity", path, sizeof(path));
    if (regular_file_read(path, buf, sizeof(buf)) != 0)
        return -1;
    unsigned long long sec = 0, usec = 0;
    char executable_uuid[40] = "";
    memset(identity, 0, sizeof(*identity));
    int fields = sscanf(buf, "%d %llu %llu %39s %c", &identity->pid,
                        &sec, &usec, executable_uuid, &extra);
    if (fields == 4) {
        if (executable_uuid_parse(executable_uuid,
                                  identity->executable_uuid) != 0)
            return -1;
        identity->has_executable_identity = 1;
    } else if (sscanf(buf, "%d %llu %llu %c", &identity->pid, &sec, &usec,
                      &extra) != 3) {
        return -1;
    }
    if (identity->pid <= 1 || sec == 0 || usec >= 1000000)
        return -1;
    identity->start_sec = (uint64_t)sec;
    identity->start_usec = (uint64_t)usec;
    return 0;
}

static int stored_identity_write(const struct profile *p,
                                 const struct process_identity *identity)
{
    char path[1024], text[192];
    profile_path(p, "vmrun.identity", path, sizeof(path));
    int n;
    if (identity->has_executable_identity) {
        char executable_uuid[33];
        proc_executable_uuid_format(identity->executable_uuid,
                                    executable_uuid);
        n = snprintf(text, sizeof(text), "%d %llu %llu %s\n", identity->pid,
                     (unsigned long long)identity->start_sec,
                     (unsigned long long)identity->start_usec,
                     executable_uuid);
    } else {
        n = snprintf(text, sizeof(text), "%d %llu %llu\n", identity->pid,
                     (unsigned long long)identity->start_sec,
                     (unsigned long long)identity->start_usec);
    }
    if (n <= 0 || n >= (int)sizeof(text))
        return -1;
    return fs_write_file_atomic(path, text, (size_t)n, 0600);
}

static int stored_identity_exists(const struct profile *p)
{
    char path[1024];
    struct stat sb;
    profile_path(p, "vmrun.identity", path, sizeof(path));
    return lstat(path, &sb) == 0;
}

static int process_looks_like_hamn(int pid)
{
    char path[PROC_PIDPATHINFO_MAXSIZE];
    int n = proc_pidpath(pid, path, sizeof(path));
    if (n <= 0)
        return -1;
    const char *base = strrchr(path, '/');
    base = base ? base + 1 : path;
    return strcmp(base, "hamn") == 0;
}

static int process_identity_alive(const struct process_identity *identity)
{
    uint64_t sec = 0, usec = 0;
    return proc_start_identity(identity->pid, &sec, &usec) == 0 &&
           identity->start_sec == sec && identity->start_usec == usec;
}

static int process_executable_identity_matches(
    const struct process_identity *identity)
{
    unsigned char uuid[16];
    return identity->has_executable_identity &&
           proc_executable_identity(identity->pid, uuid) == 0 &&
           memcmp(identity->executable_uuid, uuid, sizeof(uuid)) == 0;
}

static int process_identity_exact(const struct process_identity *identity)
{
    return process_identity_alive(identity) &&
           process_executable_identity_matches(identity);
}

static int pre_ctl_state_allows_identity(const struct profile *p)
{
    struct vm_state st;
    return state_load(p, &st) == 0 &&
           (strcmp(st.state, "starting") == 0 ||
            strcmp(st.state, "stopped") == 0);
}

static int wait_owned_pid_gone(const struct process_identity *identity,
                               int timeout_dsec);

static void spawned_identity(const struct vm_spawned_process *spawned,
                             struct process_identity *identity)
{
    memset(identity, 0, sizeof(*identity));
    identity->pid = spawned->pid;
    identity->start_sec = spawned->start_sec;
    identity->start_usec = spawned->start_usec;
    identity->has_executable_identity = 1;
    memcpy(identity->executable_uuid, spawned->executable_uuid,
           sizeof(identity->executable_uuid));
}

enum vm_spawn_process_result vm_process_capture_spawned(
    int pid, struct vm_spawned_process *spawned)
{
    if (pid <= 1 || !spawned)
        return VM_SPAWN_PROCESS_ERROR;
    memset(spawned, 0, sizeof(*spawned));
    spawned->pid = pid;

    unsigned char self_uuid[16];
    if (proc_executable_identity(getpid(), self_uuid) != 0)
        return VM_SPAWN_PROCESS_ERROR;

    for (int i = 0; i < 10; i++) {
        int status;
        pid_t waited = waitpid(pid, &status, WNOHANG);
        if (waited == pid) {
            spawned->reaped = 1;
            return VM_SPAWN_PROCESS_EXITED;
        }
        if (waited < 0) {
            if (errno == EINTR)
                continue;
            return VM_SPAWN_PROCESS_ERROR;
        }
        if (proc_start_identity(pid, &spawned->start_sec,
                                &spawned->start_usec) == 0 &&
            proc_executable_identity(pid, spawned->executable_uuid) == 0 &&
            memcmp(spawned->executable_uuid, self_uuid,
                   sizeof(self_uuid)) == 0) {
            struct process_identity identity;
            spawned->captured = 1;
            spawned_identity(spawned, &identity);
            if (process_identity_exact(&identity))
                return VM_SPAWN_PROCESS_READY;
            spawned->captured = 0;
        }
        usleep(100 * 1000);
    }
    return VM_SPAWN_PROCESS_ERROR;
}

enum vm_spawn_process_result vm_process_wait_spawned(
    const struct profile *p, struct vm_spawned_process *spawned,
    int timeout_dsec)
{
    if (!p || !spawned || !spawned->captured || spawned->reaped ||
        timeout_dsec < 1)
        return VM_SPAWN_PROCESS_ERROR;

    struct process_identity expected;
    spawned_identity(spawned, &expected);
    for (int i = 0; i < timeout_dsec; i++) {
        int status;
        pid_t waited = waitpid(spawned->pid, &status, WNOHANG);
        if (waited == spawned->pid) {
            spawned->reaped = 1;
            return VM_SPAWN_PROCESS_EXITED;
        }
        if (waited < 0) {
            if (errno == EINTR)
                continue;
            return VM_SPAWN_PROCESS_ERROR;
        }
        struct process_identity identity;
        int stored_pid = -1;
        if (stored_identity_read(p, &identity) == 0 &&
            pid_file_read(p, &stored_pid) == 0 &&
            stored_pid == spawned->pid &&
            identity.pid == expected.pid &&
            identity.start_sec == expected.start_sec &&
            identity.start_usec == expected.start_usec &&
            identity.has_executable_identity &&
            memcmp(identity.executable_uuid, expected.executable_uuid,
                   sizeof(identity.executable_uuid)) == 0 &&
            process_identity_exact(&identity) &&
            spawn_guard_probe(p) == SPAWN_GUARD_ABSENT)
            return VM_SPAWN_PROCESS_READY;
        usleep(100 * 1000);
    }
    return VM_SPAWN_PROCESS_ERROR;
}

static int wait_spawned_gone(struct vm_spawned_process *spawned,
                             int timeout_dsec)
{
    for (int i = 0; i < timeout_dsec; i++) {
        int status;
        pid_t waited = waitpid(spawned->pid, &status, WNOHANG);
        if (waited == spawned->pid) {
            spawned->reaped = 1;
            return 0;
        }
        if (waited < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        usleep(100 * 1000);
    }
    return -1;
}

int vm_process_abort_spawned(struct vm_spawned_process *spawned,
                             int timeout_dsec)
{
    if (!spawned || !spawned->captured || timeout_dsec < 1)
        return -1;
    if (spawned->reaped)
        return 0;

    int status;
    pid_t waited = waitpid(spawned->pid, &status, WNOHANG);
    if (waited == spawned->pid) {
        spawned->reaped = 1;
        return 0;
    }
    if (waited < 0)
        return -1;

    struct process_identity identity;
    spawned_identity(spawned, &identity);
    if (!process_identity_exact(&identity))
        return -1;
    if (kill(spawned->pid, SIGTERM) != 0 && errno != ESRCH)
        return -1;
    if (wait_spawned_gone(spawned, timeout_dsec) == 0)
        return 0;
    if (!process_identity_exact(&identity))
        return -1;
    if (kill(spawned->pid, SIGKILL) != 0 && errno != ESRCH)
        return -1;
    return wait_spawned_gone(spawned, 30);
}

struct ctl_status {
    int pid;
    int has_start_identity;
    uint64_t start_sec;
    uint64_t start_usec;
};

enum ctl_status_result {
    CTL_STATUS_VALID = 0,
    CTL_STATUS_UNAVAILABLE,
    CTL_STATUS_UNCERTAIN,
};

static enum ctl_status_result ctl_status_read(
    const struct profile *p, struct ctl_status *status)
{
    char ctl[1024], resp[512];
    memset(status, 0, sizeof(*status));
    profile_path(p, "vmrun.sock", ctl, sizeof(ctl));
    int query = ctlsock_query(ctl, "{\"cmd\":\"status\"}", resp,
                              sizeof(resp), 300);
    if (query == CTLSOCK_QUERY_UNAVAILABLE)
        return CTL_STATUS_UNAVAILABLE;
    if (query != CTLSOCK_QUERY_OK)
        return CTL_STATUS_UNCERTAIN;

    cJSON *j = cJSON_Parse(resp);
    if (!j)
        return CTL_STATUS_UNCERTAIN;
    const cJSON *state = cJSON_GetObjectItemCaseSensitive(j, "state");
    const cJSON *pid = cJSON_GetObjectItemCaseSensitive(j, "pid");
    const cJSON *sec = cJSON_GetObjectItemCaseSensitive(j, "start_sec");
    const cJSON *usec = cJSON_GetObjectItemCaseSensitive(j, "start_usec");
    int valid = cJSON_IsString(state) && state->valuestring[0] &&
                cJSON_IsNumber(pid) && pid->valuedouble >= 2 &&
                pid->valuedouble <= INT_MAX &&
                pid->valuedouble == (double)(int)pid->valuedouble;
    int has_sec = cJSON_IsNumber(sec);
    int has_usec = cJSON_IsNumber(usec);
    if (has_sec != has_usec)
        valid = 0;
    if (has_sec &&
        (sec->valuedouble < 0 ||
         sec->valuedouble > 9007199254740991.0 ||
         usec->valuedouble < 0 ||
         usec->valuedouble >= 1000000 ||
         sec->valuedouble != (double)(uint64_t)sec->valuedouble ||
         usec->valuedouble != (double)(uint64_t)usec->valuedouble))
        valid = 0;
    if (valid) {
        status->pid = (int)pid->valuedouble;
        status->has_start_identity = has_sec;
        status->start_sec = has_sec ? (uint64_t)sec->valuedouble : 0;
        status->start_usec = has_usec ? (uint64_t)usec->valuedouble : 0;
    }
    cJSON_Delete(j);
    return valid ? CTL_STATUS_VALID : CTL_STATUS_UNCERTAIN;
}

static int ctl_status_matches(const struct ctl_status *status,
                              const struct process_identity *identity)
{
    if (status->pid != identity->pid)
        return 0;
    if (!status->has_start_identity)
        return 1; /* legacy vmrun: active ctl + persisted OS start identity */
    return status->start_sec == identity->start_sec &&
           status->start_usec == identity->start_usec;
}

static int process_identity_same(const struct process_identity *a,
                                 const struct process_identity *b)
{
    return a->pid == b->pid && a->start_sec == b->start_sec &&
           a->start_usec == b->start_usec &&
           a->has_executable_identity == b->has_executable_identity &&
           (!a->has_executable_identity ||
            memcmp(a->executable_uuid, b->executable_uuid,
                   sizeof(a->executable_uuid)) == 0);
}

static enum vm_process_state ctl_identity_adopt(
    const struct profile *p, const struct ctl_status *status, int *pid_out)
{
    struct process_identity identity = { .pid = status->pid };
    if (proc_start_identity(status->pid, &identity.start_sec,
                            &identity.start_usec) != 0)
        return VM_PROCESS_UNVERIFIED;
    if (proc_executable_identity(status->pid,
                                 identity.executable_uuid) != 0)
        return VM_PROCESS_UNVERIFIED;
    identity.has_executable_identity = 1;
    if (status->has_start_identity &&
        (status->start_sec != identity.start_sec ||
         status->start_usec != identity.start_usec))
        return VM_PROCESS_UNVERIFIED;

    int stored_pid = -1;
    struct process_identity stored;
    int persisted = pid_file_read(p, &stored_pid) == 0 &&
                    stored_pid == identity.pid &&
                    stored_identity_read(p, &stored) == 0 &&
                    process_identity_same(&stored, &identity);
    if (!persisted) {
        if (process_looks_like_hamn(status->pid) != 1)
            return VM_PROCESS_UNVERIFIED;
        if (stored_identity_write(p, &identity) != 0 ||
            pid_file_write(p, identity.pid) != 0) {
            logerr("cannot persist recovered vmrun process identity");
            return VM_PROCESS_UNVERIFIED;
        }
    }
    if (!process_identity_alive(&identity))
        return VM_PROCESS_UNVERIFIED;
    if (pid_out)
        *pid_out = identity.pid;
    return VM_PROCESS_VERIFIED;
}

enum vm_process_state vm_process_probe(const struct profile *p, int *pid_out)
{
    int pid = -1;
    if (pid_out)
        *pid_out = -1;

    enum spawn_guard_state spawn_state = spawn_guard_probe(p);
    if (spawn_state == SPAWN_GUARD_LOCKED ||
        spawn_state == SPAWN_GUARD_UNCERTAIN)
        return VM_PROCESS_UNVERIFIED;

    struct ctl_status status;
    enum ctl_status_result ctl_result = ctl_status_read(p, &status);
    if (ctl_result == CTL_STATUS_VALID)
        return ctl_identity_adopt(p, &status, pid_out);
    if (ctl_result == CTL_STATUS_UNCERTAIN)
        return VM_PROCESS_UNVERIFIED;

    struct process_identity stored;
    int stored_valid = stored_identity_read(p, &stored) == 0;
    int pid_valid = pid_file_read(p, &pid) == 0;
    int has_live_unrelated = 0;

    if (stored_valid && process_identity_exact(&stored)) {
        if (!pre_ctl_state_allows_identity(p))
            return VM_PROCESS_UNVERIFIED;
        if (pid_valid && pid != stored.pid &&
            (kill(pid, 0) == 0 || errno == EPERM)) {
            int looks_hamn = process_looks_like_hamn(pid);
            if (looks_hamn != 0)
                return VM_PROCESS_UNVERIFIED;
        }
        if (!process_identity_exact(&stored))
            return VM_PROCESS_UNVERIFIED;
        if (pid_out)
            *pid_out = stored.pid;
        return VM_PROCESS_VERIFIED;
    }

    if (pid_valid && (kill(pid, 0) == 0 || errno == EPERM)) {
        int looks_hamn = process_looks_like_hamn(pid);
        if (looks_hamn != 0)
            return VM_PROCESS_UNVERIFIED;
        has_live_unrelated = 1;
    }

    if (stored_valid &&
        (kill(stored.pid, 0) == 0 || errno == EPERM)) {
        int looks_hamn = process_looks_like_hamn(stored.pid);
        if (looks_hamn != 0)
            return VM_PROCESS_UNVERIFIED;
        has_live_unrelated = 1;
    }
    if (!stored_valid && stored_identity_exists(p) && !has_live_unrelated)
        return VM_PROCESS_UNVERIFIED;
    return VM_PROCESS_STALE;
}

int vm_running_pid(const struct profile *p)
{
    int pid = -1;
    return vm_process_probe(p, &pid) == VM_PROCESS_VERIFIED ? pid : -1;
}

static int wait_owned_pid_gone(const struct process_identity *identity,
                               int timeout_dsec)
{
    for (int i = 0; i < timeout_dsec; i++) {
        int status;
        pid_t waited = waitpid(identity->pid, &status, WNOHANG);
        if (waited == identity->pid)
            return 0;
        if (waited < 0 && errno != ECHILD)
            return -1;
        if (!process_identity_alive(identity))
            return 0;
        usleep(100 * 1000);
    }
    return -1;
}

static int verified_identity_is(const struct profile *p,
                                const struct process_identity *identity)
{
    if (!process_identity_alive(identity) ||
        (identity->has_executable_identity &&
         !process_executable_identity_matches(identity)))
        return 0;
    struct ctl_status status;
    enum ctl_status_result result = ctl_status_read(p, &status);
    if (result == CTL_STATUS_VALID)
        return ctl_status_matches(&status, identity);
    if (result == CTL_STATUS_UNAVAILABLE)
        return pre_ctl_state_allows_identity(p) &&
               process_executable_identity_matches(identity);
    return 0;
}

static int cleanup_stopped_state(const struct profile *p)
{
    struct vm_state st;
    state_load(p, &st);
    if (docker_observer_revoke(p) != 0) {
        logerr("cannot revoke the Docker port observer");
        return VM_STOP_FORWARD_UNSAFE;
    }
    int rc = VM_STOP_OK;
    if (mount_inotify_revoke(p) != 0) {
        logerr("cannot revoke the mountInotify bridge");
        /* The VM is already stopped. Continue removing every externally
         * reachable resource so a failed lease unlink cannot leave a stale
         * socket or state file behind. */
        rc = VM_STOP_FAILED;
    }
    if (port_forward_cleanup(p, st.ip) != 0) {
        logerr("cannot clean up all host port forwards");
        rc = VM_STOP_FORWARD_UNSAFE;
    }
    ssh_master_exit(p);

    static const char *const files[] = {
        "ssh.sock", "vmrun.pid", "vmrun.pid.tmp", "vmrun.identity",
        "vmrun.identity.tmp", "vmrun.spawning", "vmrun.sock",
        "docker.sock", "agent.sock", NULL
    };
    char path[1024];
    for (int i = 0; files[i]; i++) {
        profile_path(p, files[i], path, sizeof(path));
        unlink(path);
    }

    if (restore_docker_context(p, &st) != 0 && rc == VM_STOP_OK)
        rc = VM_STOP_FAILED;
    if (kubeconfig_restore_previous(p, &st) != 0 && rc == VM_STOP_OK)
        rc = VM_STOP_FAILED;
    snprintf(st.state, sizeof(st.state), "stopped");
    if (state_save(p, &st) != 0) {
        logerr("cannot persist stopped state");
        if (rc == VM_STOP_OK)
            rc = VM_STOP_FAILED;
    }
    return rc;
}

int vm_cleanup_stale(const struct profile *p)
{
    if (vm_process_probe(p, NULL) != VM_PROCESS_STALE) {
        logerr("refusing to clean state for a live or unverified vmrun");
        return -1;
    }
    return cleanup_stopped_state(p);
}

int vm_stop(const struct profile *p, int *was_running)
{
    int pid = -1;
    enum vm_process_state process_state = vm_process_probe(p, &pid);
    if (was_running)
        *was_running = process_state == VM_PROCESS_VERIFIED;
    if (process_state == VM_PROCESS_UNVERIFIED) {
        logerr("refusing to signal an unverified vmrun process");
        return -1;
    }

    if (process_state == VM_PROCESS_VERIFIED) {
        struct process_identity identity;
        if (stored_identity_read(p, &identity) != 0 ||
            identity.pid != pid || !process_identity_alive(&identity)) {
            logerr("verified vmrun identity disappeared before stop");
            return -1;
        }
        struct vm_state st;
        state_load(p, &st);

        /* 1) 게스트 자체 종료 (가장 깨끗함) — 비동기 spawn 후 폴링 */
        if (st.ip[0] && ssh_master_alive(p) == 0) {
            logmsg("stopping guest via ssh poweroff ...");
            const char *off[] = { "sudo", "systemctl", "poweroff", NULL };
            int off_rc = ssh_exec(p, st.ip, off, 1);
            if (off_rc != 0)
                logmsg("ssh poweroff returned %d; waiting for vm exit",
                       off_rc);
            if (wait_owned_pid_gone(&identity, 250) == 0)
                goto stopped;
        }

        /* 2) vmrun SIGTERM → requestStop → 15초 유예 → 강제 */
        if (!process_identity_alive(&identity))
            goto stopped;
        if (!verified_identity_is(p, &identity)) {
            logerr("vmrun control identity is uncertain; preserving state");
            return -1;
        }
        logmsg("stopping vm (requestStop) ...");
        kill(pid, SIGTERM);
        if (wait_owned_pid_gone(&identity, 180) == 0)
            goto stopped;

        /* 3) 최후 수단 */
        if (!process_identity_alive(&identity))
            goto stopped;
        if (!verified_identity_is(p, &identity)) {
            logerr("vmrun control identity is uncertain; preserving state");
            return -1;
        }
        logerr("vm did not stop gracefully; killing");
        kill(pid, SIGKILL);
        if (wait_owned_pid_gone(&identity, 30) != 0) {
            logerr("cannot confirm vmrun termination; preserving state");
            return -1;
        }
    }

stopped:
    return cleanup_stopped_state(p);
}
