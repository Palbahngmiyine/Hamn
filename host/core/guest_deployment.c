#include "core/guest_deployment.h"

#include <CommonCrypto/CommonDigest.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include "cli.h"
#include "core/log.h"
#include "sshmgr/ssh.h"
#include "util/fs.h"

#define GUEST_DEPLOYMENT_SCHEMA_VERSION 6
#define GUEST_DEPLOYMENT_MARKER "guest-deployment.version"
#define GUEST_DEPLOYMENT_TRANSACTION_SCRIPT \
    "/usr/local/libexec/hamn/guest-deployment-transaction"
#define GUEST_DEPLOYMENT_TOKEN_CAP 33

static int deployment_fingerprint(const struct profile *profile,
                                  char output[65]);

static int marker_text(const struct profile *profile, char *text,
                       size_t capacity)
{
    char fingerprint[65];
    if (deployment_fingerprint(profile, fingerprint) != 0)
        return -1;
    int length = snprintf(text, capacity,
                          "schemaVersion=%d\nfingerprint=sha256:%s\n",
                          GUEST_DEPLOYMENT_SCHEMA_VERSION, fingerprint);
    if (length < 0 || length >= (int)capacity) {
        errno = EOVERFLOW;
        return -1;
    }
    return length;
}

int guest_deployment_is_current(const struct profile *profile)
{
    char expected[256];
    int expected_length = marker_text(profile, expected, sizeof(expected));
    if (expected_length < 0)
        return -1;

    char path[PATH_MAX];
    profile_path(profile, GUEST_DEPLOYMENT_MARKER, path, sizeof(path));
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return errno == ENOENT ? 0 : -1;

    struct stat status;
    if (fstat(fd, &status) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    if (!S_ISREG(status.st_mode) || status.st_size < 0) {
        close(fd);
        errno = EINVAL;
        return -1;
    }
    if (status.st_size > (off_t)sizeof(expected))
        return close(fd) == 0 ? 0 : -1;

    char actual[sizeof(expected)];
    size_t offset = 0;
    while (offset < (size_t)status.st_size) {
        ssize_t count = read(fd, actual + offset,
                             (size_t)status.st_size - offset);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            int saved = errno;
            close(fd);
            errno = saved;
            return -1;
        }
        if (count == 0) {
            close(fd);
            errno = EIO;
            return -1;
        }
        offset += (size_t)count;
    }
    char extra;
    ssize_t extra_count;
    do {
        extra_count = read(fd, &extra, 1);
    } while (extra_count < 0 && errno == EINTR);
    int saved = errno;
    if (close(fd) != 0 && extra_count == 0)
        return -1;
    if (extra_count != 0) {
        errno = extra_count < 0 ? saved : EOVERFLOW;
        return -1;
    }
    return offset == (size_t)expected_length &&
           memcmp(actual, expected, offset) == 0 ? 1 : 0;
}

int guest_deployment_mark_current(const struct profile *profile)
{
    char text[256];
    int length = marker_text(profile, text, sizeof(text));
    if (length < 0)
        return -1;
    char path[PATH_MAX];
    profile_path(profile, GUEST_DEPLOYMENT_MARKER, path, sizeof(path));
    return fs_write_file_atomic(path, text, (size_t)length, 0600);
}

static void fingerprint_field(CC_SHA256_CTX *context, const char *value)
{
    CC_SHA256_Update(context, value, (CC_LONG)strlen(value) + 1);
}

static int deployment_fingerprint(const struct profile *profile,
                                  char output[65])
{
    CC_SHA256_CTX context;
    CC_SHA256_Init(&context);
    fingerprint_field(&context, "hamn-immutable-guest-configuration-v1");
    /* The guest image is signed and preconfigured.  Only profile-controlled
     * runtime settings are hashed; no host source is copied into the VM. */
    fingerprint_field(&context, "docker-daemon-json");
    fingerprint_field(&context, profile->docker_daemon_json);
    fingerprint_field(&context, "rosetta");
    fingerprint_field(&context, profile->rosetta ? "true" : "false");

    unsigned char digest[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256_Final(digest, &context);
    for (int index = 0; index < CC_SHA256_DIGEST_LENGTH; index++)
        snprintf(output + index * 2, 3, "%02x", digest[index]);
    return 0;
}

static int guest_deployment_wait_cloud_init(const struct profile *profile,
                                            const char *ip,
                                            int timeout_sec)
{
    if (timeout_sec < 1) {
        errno = EINVAL;
        return -1;
    }
    char duration[32];
    if (snprintf(duration, sizeof(duration), "%ds", timeout_sec) >=
        (int)sizeof(duration)) {
        errno = EOVERFLOW;
        return -1;
    }
    const char *wait[] = {
        "sudo", "timeout", "--kill-after=5s", duration,
        "cloud-init", "status", "--wait", "--long", NULL
    };
    int rc = ssh_exec(profile, ip, wait, 0);
    if (rc == 0)
        return 0;
    if (rc == 2) {
        logmsg("warning: cloud-init completed with recoverable errors; "
               "verifying the current Hamn deployment directly");
        return 0;
    }
    if (rc == 124)
        logerr("cloud-init did not finish within %d seconds", timeout_sec);
    else
        logerr("cloud-init failed before guest deployment refresh (exit %d)",
               rc);
    return -1;
}

static int guest_deployment_configure_docker(const struct profile *profile,
                                             const char *ip)
{
    char daemon_json_env[sizeof(profile->docker_daemon_json) + 32];
    int written = snprintf(daemon_json_env, sizeof(daemon_json_env),
                           "HAMN_DOCKER_EXTRA_JSON=%s",
                           profile->docker_daemon_json);
    if (written < 0 || written >= (int)sizeof(daemon_json_env)) {
        logerr("Docker daemon configuration is too large");
        return -1;
    }
    const char *configure_docker[] = {
        "sudo", "env", daemon_json_env,
        "/usr/local/libexec/hamn/configure-docker", NULL
    };
    if (ssh_exec(profile, ip, configure_docker, 0) != 0) {
        logerr("Docker daemon configuration failed; "
               "the guest image must contain Docker Engine");
        return -1;
    }
    return 0;
}

static int guest_deployment_configure_rosetta(const struct profile *profile,
                                              const char *ip)
{
    const char *action = profile->rosetta ? "enable" : "disable";
    const char *configure_rosetta[] = {
        "sudo", "/usr/local/libexec/hamn/configure-rosetta", action, NULL
    };
    if (ssh_exec(profile, ip, configure_rosetta, 0) != 0) {
        logerr("cannot configure %s x86_64 translation",
               profile->rosetta ? "Rosetta" : "qemu");
        return -1;
    }
    return 0;
}

static int guest_deployment_verify_image(const struct profile *profile,
                                         const char *ip)
{
    const char *mode = profile->rosetta ? "rosetta" : "qemu";
    char mode_env[32];
    int written = snprintf(mode_env, sizeof(mode_env),
                           "HAMN_BINFMT_MODE=%s", mode);
    if (written < 0 || written >= (int)sizeof(mode_env)) {
        logerr("cannot construct guest binfmt verification mode");
        return -1;
    }
    const char *verify_image[] = {
        "sudo", "env", mode_env,
        "/usr/local/libexec/hamn/verify-image-contract", NULL
    };
    if (ssh_exec(profile, ip, verify_image, 0) != 0) {
        logerr("guest image does not satisfy Hamn's preconfigured Docker/CRI contract");
        return -1;
    }
    return 0;
}

int guest_deployment_configure_runtime(const struct profile *profile,
                                       const char *ip)
{
    if (guest_deployment_configure_rosetta(profile, ip) != 0 ||
        guest_deployment_verify_image(profile, ip) != 0)
        return -1;
    const char *configure_containerd[] = {
        "sudo", "/usr/local/libexec/hamn/configure-containerd", NULL
    };
    if (ssh_exec(profile, ip, configure_containerd, 0) != 0) {
        logerr("system containerd CRI configuration failed; "
               "the existing VM may need reprovisioning");
        return -1;
    }
    return guest_deployment_configure_docker(profile, ip);
}

static int replace_unix_forward(const struct profile *profile,
                                const char *ip, const char *local_socket,
                                const char *remote_socket)
{
    (void)ssh_forward_cancel_unix(profile, ip, local_socket, remote_socket);
    return ssh_forward_add_unix(profile, ip, local_socket, remote_socket);
}

int guest_deployment_forward_sockets(const struct profile *profile,
                                     const char *ip)
{
    char agent_socket[PATH_MAX];
    profile_path(profile, "agent.sock", agent_socket, sizeof(agent_socket));
    if (replace_unix_forward(profile, ip, agent_socket,
                             "/run/hamnd.sock") != 0) {
        logerr("cannot forward hamnd agent socket");
        return -1;
    }

    char docker_socket[PATH_MAX];
    profile_path(profile, "docker.sock", docker_socket,
                 sizeof(docker_socket));
    if (replace_unix_forward(profile, ip, docker_socket,
                             "/var/run/docker.sock") != 0) {
        logerr("cannot forward the guest Docker socket");
        return -1;
    }
    return 0;
}

static int unix_socket_connectable(const char *path)
{
    if (strlen(path) >= sizeof(((struct sockaddr_un *)0)->sun_path))
        return 0;
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return 0;
    struct sockaddr_un address;
    memset(&address, 0, sizeof(address));
    address.sun_family = AF_UNIX;
    snprintf(address.sun_path, sizeof(address.sun_path), "%s", path);
    int ready = connect(fd, (struct sockaddr *)&address,
                        sizeof(address)) == 0;
    close(fd);
    return ready;
}

static int runtime_ready_once(const struct profile *profile, const char *ip)
{
    char docker_socket[PATH_MAX];
    profile_path(profile, "docker.sock", docker_socket,
                 sizeof(docker_socket));
    if (!unix_socket_connectable(docker_socket))
        return 0;

    const char *agent_probe[] = {
        "sudo", "systemctl", "is-active", "--quiet", "hamnd.service",
        NULL
    };
    const char *docker_probe[] = {
        "sudo", "docker", "version", "--format", "{{.Server.Version}}",
        NULL
    };
    const char *containerd_probe[] = {
        "sudo", "ctr", "--address", "/run/containerd/containerd.sock",
        "version", NULL
    };
    return ssh_exec(profile, ip, agent_probe, 1) == 0 &&
           ssh_exec(profile, ip, docker_probe, 1) == 0 &&
           ssh_exec(profile, ip, containerd_probe, 1) == 0;
}

static long long monotonic_milliseconds(void)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return -1;
    return (long long)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

int guest_deployment_runtime_ready(const struct profile *profile,
                                   const char *ip, int timeout_sec)
{
    long long started = monotonic_milliseconds();
    if (started < 0)
        return -1;
    long long deadline = started + (long long)timeout_sec * 1000;
    long delay_ms = 50;
    for (;;) {
        if (runtime_ready_once(profile, ip))
            return 0;
        long long now = monotonic_milliseconds();
        if (now < 0 || now >= deadline)
            return -1;
        long long remaining = deadline - now;
        long sleep_ms = delay_ms < remaining ? delay_ms : (long)remaining;
        struct timespec delay = {
            .tv_sec = sleep_ms / 1000,
            .tv_nsec = (sleep_ms % 1000) * 1000000,
        };
        while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {}
        if (delay_ms < 250)
            delay_ms *= 2;
    }
}

static void deployment_token_generate(
    char token[GUEST_DEPLOYMENT_TOKEN_CAP])
{
    unsigned char bytes[16];
    arc4random_buf(bytes, sizeof(bytes));
    for (size_t index = 0; index < sizeof(bytes); index++)
        snprintf(token + index * 2, GUEST_DEPLOYMENT_TOKEN_CAP - index * 2,
                 "%02x", bytes[index]);
}

static int deployment_transaction(const struct profile *profile,
                                  const char *ip, const char *action,
                                  const char *token)
{
    const char *command[] = {
        "sudo", "bash", GUEST_DEPLOYMENT_TRANSACTION_SCRIPT, action, token,
        NULL
    };
    return ssh_exec(profile, ip, command, 0);
}

static int deployment_refresh_locked(const struct profile *profile,
                                     const struct vm_state *state, int force)
{
    int current = guest_deployment_is_current(profile);
    if (current == 1 && !force)
        return 0;
    if (current < 0) {
        logerr("cannot validate guest deployment marker: %s",
               strerror(errno));
        return -1;
    }
    if (!state || !state->ip[0]) {
        errno = EINVAL;
        logerr("cannot refresh guest deployment without a running VM address");
        return -1;
    }

    logmsg("guest image configuration %s; applying helpers and forwards ...",
           force ? "failed readiness" : "fingerprint changed");
    if (ssh_master_start(profile, state->ip, 15) != 0 ||
        guest_deployment_wait_cloud_init(profile, state->ip, 600) != 0)
        return -1;

    char token[GUEST_DEPLOYMENT_TOKEN_CAP];
    deployment_token_generate(token);
    if (deployment_transaction(profile, state->ip, "begin", token) != 0) {
        logerr("cannot create a safe guest deployment backup");
        return -1;
    }

    int forward_attempted = 0;
    if (guest_deployment_configure_runtime(profile, state->ip) != 0)
        goto rollback;
    forward_attempted = 1;
    if (guest_deployment_forward_sockets(profile, state->ip) != 0)
        goto rollback;
    if (guest_deployment_runtime_ready(profile, state->ip, 30) != 0) {
        logerr("guest agent, Docker, or system containerd did not become ready");
        goto rollback;
    }
    if (deployment_transaction(profile, state->ip, "commit", token) != 0) {
        logerr("guest deployment succeeded but backup commit failed for "
               "operation %s; the host marker was not updated", token);
        return -1;
    }
    if (guest_deployment_mark_current(profile) != 0) {
        logerr("guest deployment was committed but the host version marker "
               "could not be persisted: %s; the next refresh will retry "
               "the deployment safely", strerror(errno));
        return -1;
    }
    logmsg("guest deployment fingerprint is current");
    return 0;

rollback:
    logerr("guest deployment failed; restoring the previous guest runtime");
    if (deployment_transaction(profile, state->ip, "rollback", token) != 0) {
        logerr("guest deployment rollback failed for operation %s; "
               "the guest backup was retained for manual recovery", token);
        return -1;
    }
    logmsg("previous guest runtime restored after failed deployment");
    if (forward_attempted &&
        guest_deployment_forward_sockets(profile, state->ip) != 0)
        logerr("guest runtime was restored but its host socket forwards "
               "could not be re-established");
    return -1;
}

int guest_deployment_refresh_locked(const struct profile *profile,
                                    const struct vm_state *state)
{
    return deployment_refresh_locked(profile, state, 0);
}

int guest_deployment_reconcile_runtime_locked(
    const struct profile *profile, const struct vm_state *state)
{
    if (guest_deployment_is_current(profile) != 1) {
        errno = ESTALE;
        logerr("cannot reconcile a stale guest deployment");
        return -1;
    }
    if (!state || !state->ip[0]) {
        errno = EINVAL;
        logerr("cannot reconcile guest runtime without a running VM address");
        return -1;
    }

    char token[GUEST_DEPLOYMENT_TOKEN_CAP];
    deployment_token_generate(token);
    if (deployment_transaction(profile, state->ip, "begin", token) != 0) {
        logerr("cannot create a safe guest runtime reconciliation backup");
        return -1;
    }

    if (guest_deployment_configure_docker(profile, state->ip) == 0 &&
        guest_deployment_forward_sockets(profile, state->ip) == 0 &&
        guest_deployment_runtime_ready(profile, state->ip, 30) == 0) {
        if (deployment_transaction(profile, state->ip, "commit", token) == 0) {
            logmsg("warm start reconciled Docker networking and readiness");
            return 0;
        }
        logerr("guest runtime reconciliation succeeded but backup commit failed "
               "for operation %s", token);
        return -1;
    }

    logerr("warm start reconciliation failed; restoring the previous guest runtime");
    if (deployment_transaction(profile, state->ip, "rollback", token) != 0) {
        logerr("guest runtime reconciliation rollback failed for operation %s; "
               "the guest backup was retained for manual recovery", token);
        return -1;
    }
    if (guest_deployment_forward_sockets(profile, state->ip) != 0)
        logerr("guest runtime was restored but its host socket forwards "
               "could not be re-established");
    return -1;
}

int guest_deployment_repair_locked(const struct profile *profile,
                                   const struct vm_state *state)
{
    return deployment_refresh_locked(profile, state, 1);
}
