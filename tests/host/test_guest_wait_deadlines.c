/* The guest waits of a VM start end at their deadline or on cancellation:
 * every probe of guest_deployment_runtime_ready() runs with the readiness
 * budget that is left, the guest image check has its own bound, and the DHCP
 * lease wait ends when the operation is cancelled. Each case runs in a child
 * that this test kills when it outlives its limit, so a wait without a
 * deadline fails its case instead of hanging the test.
 *
 * `ssh` is this test executable, first in PATH. It answers control requests
 * (-O) with success, links a forwarded Docker socket to a fake Docker API,
 * and runs no remote command: the command succeeds, fails, is slow or hangs
 * as the case's environment asks. It can also stand for a guest with one
 * deployment transaction that an earlier operation left: the helper's begin
 * refuses while it exists, and the recovery command discards it. The fake
 * Docker API answers _ping, or accepts and never answers. The Makefile builds
 * guest_deployment.c into this test with readiness and image-check budgets
 * of seconds. */
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "core/guest_deployment.h"
#include "core/log.h"
#include "core/profile.h"
#include "util/dhcp_leases.h"
#include "util/proc.h"

#if !defined(GUEST_DEPLOYMENT_READY_TIMEOUT_SEC) || \
    !defined(GUEST_IMAGE_VERIFY_TIMEOUT_SEC)
#error "build with the test budgets of guest_deployment.c (see Makefile)"
#endif

#define READY_MS (GUEST_DEPLOYMENT_READY_TIMEOUT_SEC * 1000LL)
#define VERIFY_MS (GUEST_IMAGE_VERIFY_TIMEOUT_SEC * 1000LL)
/* Reaping the exact client after its deadline takes a few polls of 50 ms. */
#define MARGIN_MS 700LL
/* A case still running this long after its budget has a wait without one. */
#define WATCHDOG_EXTRA_MS 6000LL
#define GUEST_IP "192.0.2.10"              /* TEST-NET-1; ssh here is fake */
#define LEASELESS_MAC "00:00:5e:00:53:01"  /* RFC 7042 documentation MAC */

#define ENV_LOG "HAMN_TEST_FAKE_SSH_LOG"
#define ENV_API "HAMN_TEST_FAKE_DOCKER_API"
/* Substrings of the remote command that make the fake ssh hang or fail. */
#define ENV_HANG "HAMN_TEST_FAKE_SSH_HANG"
#define ENV_FAIL "HAMN_TEST_FAKE_SSH_FAIL"
/* Milliseconds every remote command takes before it answers. */
#define ENV_DELAY "HAMN_TEST_FAKE_SSH_DELAY_MS"
/* The operation's process, sent SIGTERM when the hanging command starts, as
 * the TUI cancels its worker. */
#define ENV_CANCEL "HAMN_TEST_FAKE_SSH_CANCEL_PID"
/* A file that stands for the leftover transaction while it exists. */
#define ENV_LEFTOVER "HAMN_TEST_FAKE_GUEST_LEFTOVER"
#define RECOVERY_ROOT "/var/lib/hamn/deployment-transactions"
#define TRANSACTION_HELPER "/usr/local/libexec/hamn/guest-deployment-transaction"

static long long now_ms(void)
{
    struct timespec now;
    assert(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
    return (long long)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static void sleep_ms(long milliseconds)
{
    struct timespec delay = {
        .tv_sec = milliseconds / 1000,
        .tv_nsec = (milliseconds % 1000) * 1000000,
    };
    while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {}
}

/* ---- the fake ssh ---------------------------------------------------- */

/* One line per invocation: "<pid>\t<remote command or -O request>". */
static void fake_log(const char *text)
{
    const char *path = getenv(ENV_LOG);
    if (!path)
        return;
    size_t length = strlen(text);
    char *line = malloc(length + 32);
    if (!line)
        _exit(254);
    int prefix = snprintf(line, 32, "%d\t", (int)getpid());
    for (size_t i = 0; i < length; i++)
        line[prefix + i] = text[i] == '\n' || text[i] == '\r' ? ' ' : text[i];
    line[prefix + length] = '\n';
    int fd = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0 || proc_write_all(fd, line, (size_t)prefix + length + 1) != 0)
        _exit(254);
    close(fd);
    free(line);
}

static int fake_matches(const char *variable, const char *command)
{
    const char *needle = getenv(variable);
    return needle && needle[0] && strstr(command, needle);
}

/* `-O forward -L <local>:<remote>`: the Docker socket forward becomes a link
 * to the fake Docker API, as the real master creates the local socket. */
static int fake_forward(int argc, char **argv)
{
    for (int i = 1; i + 1 < argc; i++) {
        if (strcmp(argv[i], "-L") != 0)
            continue;
        char spec[2 * PATH_MAX];
        snprintf(spec, sizeof(spec), "%s", argv[i + 1]);
        char *colon = strrchr(spec, ':');
        if (!colon)
            return 255;
        *colon = '\0';
        const char *api = getenv(ENV_API);
        if (api && strcmp(colon + 1, "/var/run/docker.sock") == 0) {
            (void)unlink(spec);
            return symlink(api, spec) == 0 ? 0 : 255;
        }
        return 0;
    }
    return 255;
}

static int fake_ssh(int argc, char **argv)
{
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "-O") != 0)
            continue;
        if (i + 1 >= argc)
            return 255;
        char request[64];
        snprintf(request, sizeof(request), "-O %s", argv[i + 1]);
        fake_log(request);
        return strcmp(argv[i + 1], "forward") == 0 ?
            fake_forward(argc, argv) : 0;
    }
    const char *command = argv[argc - 1];
    fake_log(command);
    const char *delay = getenv(ENV_DELAY);
    if (delay)
        sleep_ms(atol(delay));
    if (fake_matches(ENV_HANG, command)) {
        const char *owner = getenv(ENV_CANCEL);
        if (owner && kill((pid_t)atoi(owner), SIGTERM) != 0)
            return 254;
        for (;;)
            pause();
    }
    if (fake_matches(ENV_FAIL, command))
        return 3;
    /* What the helper and the recovery script do with a leftover entry whose
     * phase is not ready. */
    const char *leftover = getenv(ENV_LEFTOVER);
    if (leftover && access(leftover, F_OK) == 0) {
        if (strstr(command, TRANSACTION_HELPER " begin")) {
            fputs("guest-deployment-transaction: deployment transaction "
                  "0123456789abcdef0123456789abcdef has no ready backup; it is "
                  "never restored, and deployment recovery discards it\n",
                  stderr);
            return 1;
        }
        if (strstr(command, " " RECOVERY_ROOT " ")) {
            if (unlink(leftover) != 0)
                return 254;
            fputs("discarded deployment backup "
                  "0123456789abcdef0123456789abcdef (phase not ready): it was "
                  "never published or was already retired, so it is never "
                  "restored\n", stderr);
            return 0;
        }
    }
    if (strstr(command, "verify-image-contract"))
        puts("hamn: verified Ubuntu 24.04 arm64 guest image contract");
    return 0;
}

/* ---- the fixture ----------------------------------------------------- */

static struct {
    char root[64];
    char bin[128];
    char api[128];
    char log[128];
    char out[128];
    char leftover[128];
    char docker_socket[256];
    struct profile profile;
} fixture;

enum api_mode { API_ANSWERS, API_SILENT };

/* A fake Docker API on fixture.api, listening before this returns. */
static pid_t api_start(enum api_mode mode)
{
    (void)unlink(fixture.api);
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    assert(strlen(fixture.api) < sizeof(address.sun_path));
    snprintf(address.sun_path, sizeof(address.sun_path), "%s", fixture.api);
    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    assert(listener >= 0);
    assert(bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0);
    assert(listen(listener, 16) == 0);
    pid_t server = fork();
    assert(server >= 0);
    if (server == 0) {
        signal(SIGPIPE, SIG_IGN);
        for (;;) {
            int connection = accept(listener, NULL, NULL);
            if (connection < 0) {
                if (errno == EINTR)
                    continue;
                _exit(1);
            }
            if (mode == API_SILENT)
                continue; /* held open and never answered */
            char request[4096];
            (void)read(connection, request, sizeof(request));
            static const char reply[] =
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n"
                "Content-Length: 2\r\nConnection: close\r\n\r\nOK";
            (void)proc_write_all(connection, reply, sizeof(reply) - 1);
            close(connection);
        }
    }
    close(listener);
    return server;
}

static char *read_file(const char *path)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        assert(errno == ENOENT);
        char *empty = calloc(1, 1);
        assert(empty);
        return empty;
    }
    struct stat status;
    assert(fstat(fd, &status) == 0);
    char *text = malloc((size_t)status.st_size + 1);
    assert(text);
    assert(proc_read_all(fd, text, (size_t)status.st_size) == 0);
    text[status.st_size] = '\0';
    close(fd);
    return text;
}

/* The fake ssh invocations whose command contains needle. */
static int ssh_calls(const char *needle)
{
    char *text = read_file(fixture.log);
    int count = 0;
    for (char *line = text; *line;) {
        char *end = strchr(line, '\n');
        assert(end);
        *end = '\0';
        char *command = strchr(line, '\t');
        assert(command);
        if (strstr(command + 1, needle))
            count++;
        line = end + 1;
    }
    free(text);
    return count;
}

/* Every fake ssh gone after waiting at most limit_ms. */
static int ssh_clients_gone(long long limit_ms)
{
    char *text = read_file(fixture.log);
    long long deadline = now_ms() + limit_ms;
    int gone = 1;
    for (char *line = text; *line && gone;) {
        pid_t pid = (pid_t)strtol(line, NULL, 10);
        while (kill(pid, 0) == 0 && now_ms() < deadline)
            sleep_ms(20);
        gone = kill(pid, 0) != 0 && errno == ESRCH;
        char *end = strchr(line, '\n');
        line = end ? end + 1 : line + strlen(line);
    }
    free(text);
    return gone;
}

/* The call returned only after reaping the exact client it started. */
static void ssh_clients_reaped(void)
{
    assert(ssh_clients_gone(0));
}

static int failures;

static void run_case(const char *name, void (*body)(void),
                     enum api_mode api, long long limit_ms)
{
    int fd = open(fixture.log, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    assert(fd >= 0 && close(fd) == 0);
    (void)unlink(fixture.docker_socket);
    assert(symlink(fixture.api, fixture.docker_socket) == 0);
    pid_t server = api_start(api);

    fflush(NULL);
    long long started = now_ms();
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        int out = open(fixture.out, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND |
                       O_CLOEXEC, 0600);
        if (out < 0 || dup2(out, STDOUT_FILENO) < 0 ||
            dup2(out, STDERR_FILENO) < 0)
            _exit(125);
        close(out);
        body();
        fflush(NULL);
        _exit(0);
    }
    int status = 0;
    pid_t waited = 0;
    while ((waited = waitpid(child, &status, WNOHANG)) == 0 &&
           now_ms() - started < limit_ms)
        sleep_ms(20);
    int finished = waited == child;
    if (!finished) {
        assert(waited == 0 && kill(child, SIGKILL) == 0);
        while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
    }
    long long elapsed = now_ms() - started;
    assert(kill(server, SIGKILL) == 0);
    while (waitpid(server, NULL, 0) < 0 && errno == EINTR) {}
    /* A killed case leaves its fake ssh to the supervisor's owner-death
     * policy, which this test shortens to 50 ms of grace. */
    int gone = ssh_clients_gone(3000);

    if (finished && WIFEXITED(status) && WEXITSTATUS(status) == 0 && gone) {
        printf("ok: %s (%lld ms)\n", name, elapsed);
        return;
    }
    failures++;
    if (!finished)
        fprintf(stderr, "FAIL: %s: still waiting after %lld ms; the wait "
                "has no deadline\n", name, elapsed);
    else if (!gone)
        fprintf(stderr, "FAIL: %s: a fake ssh outlived the case\n", name);
    else
        fprintf(stderr, "FAIL: %s: wait status %d after %lld ms\n", name,
                status, elapsed);
    char *output = read_file(fixture.out);
    fprintf(stderr, "--- output of %s ---\n%s--- end ---\n", name, output);
    free(output);
}

/* ---- readiness ------------------------------------------------------- */

static void ready_when_every_probe_answers(void)
{
    assert(guest_deployment_runtime_ready(&fixture.profile, GUEST_IP, 2) == 0);
    assert(ssh_calls("sudo systemctl is-active --quiet hamnd.service") == 1);
    assert(ssh_calls("sudo docker version --format") == 1);
    assert(ssh_calls("sudo ctr --address /run/containerd/containerd.sock "
                     "version") == 1);
    ssh_clients_reaped();
}

static void a_failing_probe_is_not_ready(void)
{
    setenv(ENV_FAIL, "ctr --address", 1);
    long long started = now_ms();
    assert(guest_deployment_runtime_ready(&fixture.profile, GUEST_IP, 1) == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= 1000 && elapsed < 1000 + MARGIN_MS);
    assert(ssh_calls("ctr --address") >= 2); /* polled until the budget ended */
    ssh_clients_reaped();
}

static void hung_probe_ends_at_the_budget(const char *probe)
{
    setenv(ENV_HANG, probe, 1);
    long long started = now_ms();
    assert(guest_deployment_runtime_ready(&fixture.profile, GUEST_IP, 2) == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= 2000 && elapsed < 2000 + MARGIN_MS);
    assert(ssh_calls(probe) == 1);
    ssh_clients_reaped();
}

static void hung_agent_probe_ends_at_the_budget(void)
{
    hung_probe_ends_at_the_budget("systemctl is-active");
}

static void hung_docker_probe_ends_at_the_budget(void)
{
    hung_probe_ends_at_the_budget("docker version");
}

static void hung_containerd_probe_ends_at_the_budget(void)
{
    hung_probe_ends_at_the_budget("ctr --address");
}

/* curl's own --max-time is 2 seconds; the readiness budget here is 1. */
static void silent_docker_api_ends_at_the_budget(void)
{
    long long started = now_ms();
    assert(guest_deployment_runtime_ready(&fixture.profile, GUEST_IP, 1) == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= 1000 && elapsed < 1000 + MARGIN_MS);
    assert(ssh_calls("sudo") == 0);
}

/* The budget is a deadline: a round that does not fit in it is not ready,
 * and the budget of a VM that is already running holds the round of a
 * healthy guest that answers slowly. */
static void slow_guest_is_ready_within_the_running_vm_budget(void)
{
    setenv(ENV_DELAY, "500", 1);
    long long started = now_ms();
    assert(guest_deployment_runtime_ready(&fixture.profile, GUEST_IP, 1) == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= 1000 && elapsed < 1000 + MARGIN_MS);
    assert(guest_deployment_runtime_ready(
               &fixture.profile, GUEST_IP,
               GUEST_DEPLOYMENT_RUNNING_READY_TIMEOUT_SEC) == 0);
    ssh_clients_reaped();
}

static void cancellation_ends_the_readiness_wait(void)
{
    assert(proc_cancel_install() == 0);
    char owner[16];
    snprintf(owner, sizeof(owner), "%d", (int)getpid());
    setenv(ENV_CANCEL, owner, 1);
    setenv(ENV_HANG, "systemctl is-active", 1);
    long long started = now_ms();
    errno = 0;
    int rc = guest_deployment_runtime_ready(&fixture.profile, GUEST_IP, 20);
    int saved = errno;
    long long elapsed = now_ms() - started;
    assert(rc == -1 && saved == ECANCELED && proc_cancelled());
    assert(elapsed < 2000); /* of a 20-second budget */
    assert(ssh_calls("sudo") == 1); /* no probe after the cancellation */
    ssh_clients_reaped();
}

/* A cancelled recovery runs again inside cleanup, restores the socket
 * forwards and checks readiness; a probe that hangs there ends with the
 * readiness budget, so the cancelled operation can still exit. */
static void hang_in_cleanup_after_cancellation_ends_at_the_budget(void)
{
    assert(proc_cancel_install() == 0);
    assert(raise(SIGTERM) == 0 && proc_cancelled());
    setenv(ENV_HANG, "systemctl is-active", 1);
    long long started = now_ms();
    int rc = guest_deployment_recover(&fixture.profile, GUEST_IP);
    long long elapsed = now_ms() - started;
    assert(rc != 0 && !guest_deployment_recovery_complete());
    assert(elapsed >= READY_MS && elapsed < READY_MS + 2000);
    /* The first recovery was refused before it started; its retry ran. */
    assert(ssh_calls("/var/lib/hamn/deployment-transactions") == 1);
    assert(ssh_calls("-O forward") == 2);
    assert(ssh_calls("systemctl is-active") == 1);
    assert(proc_cancelled());
    ssh_clients_reaped();
}

/* ---- reconciliation over a leftover transaction --------------------- */

/* A current marker and a guest with a leftover transaction. */
static void leave_a_transaction(struct vm_state *state)
{
    assert(guest_deployment_mark_current(&fixture.profile) == 0);
    int fd = open(fixture.leftover, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC,
                  0600);
    assert(fd >= 0 && close(fd) == 0);
    assert(setenv(ENV_LEFTOVER, fixture.leftover, 1) == 0);
    memset(state, 0, sizeof(*state));
    snprintf(state->ip, sizeof(state->ip), "%s", GUEST_IP);
}

static int leftover_exists(void)
{
    if (access(fixture.leftover, F_OK) == 0)
        return 1;
    assert(errno == ENOENT);
    return 0;
}

/* A cold start without a leftover: the recovery finds nothing, and only
 * Docker's network-dependent settings are applied. */
static void reconcile_without_a_leftover_configures_docker_only(void)
{
    struct vm_state state;
    leave_a_transaction(&state);
    assert(unsetenv(ENV_LEFTOVER) == 0 && unlink(fixture.leftover) == 0);
    assert(guest_deployment_reconcile_runtime_locked(&fixture.profile,
                                                     &state) == 0);
    assert(ssh_calls(" " RECOVERY_ROOT " ") == 1);
    assert(ssh_calls(TRANSACTION_HELPER " begin") == 1);
    assert(ssh_calls("configure-docker") == 1);
    assert(ssh_calls(TRANSACTION_HELPER " commit") == 1);
    assert(ssh_calls("configure-rosetta") == 0 &&
           ssh_calls("verify-image-contract") == 0 &&
           ssh_calls("configure-containerd") == 0 && ssh_calls("cloud-init") == 0);
    fflush(NULL);
    char *output = read_file(fixture.out);
    assert(!strstr(output, "deployment recovery"));
    assert(!strstr(output, "whole guest runtime"));
    assert(strstr(output, "warm start reconciled Docker networking"));
    free(output);
    ssh_clients_reaped();
}

/* A crash can leave a transaction that the helper's begin refuses. The
 * reconciliation of a cold start recovers it before its begin and succeeds,
 * so the start needs no repair. Because the recovery changed the guest, and
 * on an image without the durable commit the crash may also have lost the
 * last deployment's writes, the whole runtime is configured again inside the
 * reconciliation's own transaction: x86_64 translation, the image check,
 * containerd and Docker, each once. Only the refresh's cloud-init wait,
 * which a disk past its first boot does not need, is left out. */
static void reconcile_recovers_a_leftover_and_applies_the_whole_runtime(void)
{
    struct vm_state state;
    leave_a_transaction(&state);
    assert(guest_deployment_reconcile_runtime_locked(&fixture.profile,
                                                     &state) == 0);
    assert(!leftover_exists());
    assert(ssh_calls(" " RECOVERY_ROOT " ") == 1);
    assert(ssh_calls(TRANSACTION_HELPER " begin") == 1);
    assert(ssh_calls("configure-rosetta disable") == 1);
    assert(ssh_calls("verify-image-contract") == 1);
    assert(ssh_calls("configure-containerd") == 1);
    assert(ssh_calls("configure-docker") == 1);
    assert(ssh_calls(TRANSACTION_HELPER " commit") == 1);
    assert(ssh_calls(TRANSACTION_HELPER " rollback") == 0);
    assert(ssh_calls("cloud-init") == 0);
    assert(!guest_deployment_cleanup_pending());
    fflush(NULL);
    char *output = read_file(fixture.out);
    assert(strstr(output, "deployment recovery: discarded deployment backup "
                          "0123456789abcdef0123456789abcdef (phase not ready)"));
    assert(strstr(output, "applying the whole guest runtime configuration"));
    assert(strstr(output, "warm start applied the whole guest runtime"));
    assert(!strstr(output, "has no ready backup"));
    assert(!strstr(output, "cannot create a safe guest runtime"));
    assert(!strstr(output, "reconciliation failed"));
    free(output);
    ssh_clients_reaped();
}

/* A recovery that fails begins nothing: the guest keeps its runtime and the
 * leftover stays for the next attempt. */
static void reconcile_begins_nothing_when_recovery_fails(void)
{
    struct vm_state state;
    leave_a_transaction(&state);
    setenv(ENV_FAIL, " " RECOVERY_ROOT " ", 1);
    assert(guest_deployment_reconcile_runtime_locked(&fixture.profile,
                                                     &state) == -1);
    assert(strstr(log_last_error(), "deployment recovery failed"));
    assert(leftover_exists());
    assert(ssh_calls(" " RECOVERY_ROOT " ") == 1);
    assert(ssh_calls(TRANSACTION_HELPER " ") == 0);
    assert(ssh_calls("configure-docker") == 0 && ssh_calls("-O forward") == 0);
    assert(!guest_deployment_cleanup_pending() &&
           !guest_deployment_recovery_complete());
    ssh_clients_reaped();
}

/* A recovery cancelled before its start runs again inside cleanup. The
 * reconciliation then fails without attempting its begin, names the
 * interrupted recovery as the reason, and keeps what the retry reported: it
 * finished and Docker is ready. */
static void cancelled_recovery_keeps_its_result_in_reconcile(void)
{
    struct vm_state state;
    leave_a_transaction(&state);
    assert(proc_cancel_install() == 0);
    assert(raise(SIGTERM) == 0 && proc_cancelled());
    assert(guest_deployment_reconcile_runtime_locked(&fixture.profile,
                                                     &state) == -1);
    assert(strstr(log_last_error(), "deployment recovery failed"));
    assert(!leftover_exists());
    assert(ssh_calls(" " RECOVERY_ROOT " ") == 1);
    assert(ssh_calls(TRANSACTION_HELPER " ") == 0);
    assert(ssh_calls("configure-docker") == 0);
    assert(guest_deployment_recovery_complete() &&
           !guest_deployment_cleanup_pending());
    assert(proc_cancelled());
    ssh_clients_reaped();
}

/* ---- guest image check ---------------------------------------------- */

static void hung_image_check_ends_at_its_bound(void)
{
    setenv(ENV_HANG, "verify-image-contract", 1);
    long long started = now_ms();
    assert(guest_deployment_configure_runtime(&fixture.profile, GUEST_IP) == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= VERIFY_MS && elapsed < VERIFY_MS + 2000);
    assert(strstr(log_last_error(), "did not finish within"));
    assert(ssh_calls("configure-rosetta") == 1);
    assert(ssh_calls("verify-image-contract") == 1);
    assert(ssh_calls("configure-containerd") == 0);
    ssh_clients_reaped();
}

/* The check's own report still reaches the operation's output. */
static void image_check_output_stays_visible(void)
{
    assert(guest_deployment_configure_runtime(&fixture.profile, GUEST_IP) == 0);
    assert(ssh_calls("verify-image-contract") == 1);
    assert(ssh_calls("configure-containerd") == 1);
    assert(ssh_calls("configure-docker") == 1);
    fflush(NULL);
    char *output = read_file(fixture.out);
    assert(strstr(output,
                  "hamn: verified Ubuntu 24.04 arm64 guest image contract"));
    free(output);
    ssh_clients_reaped();
}

/* ---- DHCP lease wait ------------------------------------------------- */

static void cancelled_lease_wait_returns_at_once(void)
{
    assert(proc_cancel_install() == 0);
    assert(raise(SIGTERM) == 0 && proc_cancelled());
    char ip[64];
    long long started = now_ms();
    errno = 0;
    assert(dhcp_wait_ip(LEASELESS_MAC, 120, ip, sizeof(ip)) == -1);
    assert(errno == ECANCELED);
    assert(now_ms() - started < 500);
}

static void cancellation_ends_the_lease_wait(void)
{
    assert(proc_cancel_install() == 0);
    int go[2];
    assert(pipe(go) == 0);
    pid_t owner = getpid();
    pid_t canceller = fork();
    assert(canceller >= 0);
    if (canceller == 0) {
        char byte;
        close(go[1]);
        if (read(go[0], &byte, 1) != 1)
            _exit(1);
        /* So the signal lands while the wait sleeps between lease reads. */
        sleep_ms(300);
        _exit(kill(owner, SIGTERM) == 0 ? 0 : 1);
    }
    close(go[0]);
    char ip[64];
    long long started = now_ms();
    assert(write(go[1], "g", 1) == 1);
    errno = 0;
    int rc = dhcp_wait_ip(LEASELESS_MAC, 120, ip, sizeof(ip));
    int saved = errno;
    long long elapsed = now_ms() - started;
    int status = 0;
    while (waitpid(canceller, &status, 0) < 0 && errno == EINTR) {}
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(rc == -1 && saved == ECANCELED);
    assert(elapsed < 1500);
}

static void lease_wait_without_a_lease_times_out(void)
{
    char ip[64];
    long long started = now_ms();
    assert(dhcp_wait_ip(LEASELESS_MAC, 1, ip, sizeof(ip)) == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= 1000 && elapsed < 3000);
    assert(!proc_cancelled());
}

static void join(char *output, size_t capacity, const char *name)
{
    int length = snprintf(output, capacity, "%s/%s", fixture.root, name);
    assert(length > 0 && (size_t)length < capacity);
}

int main(int argc, char **argv)
{
    const char *name = strrchr(argv[0], '/');
    if (strcmp(name ? name + 1 : argv[0], "ssh") == 0)
        return fake_ssh(argc, argv);

    char self[PATH_MAX];
    assert(proc_self_path(self, sizeof(self)));
    snprintf(fixture.root, sizeof(fixture.root), "/tmp/hamn-guest-waits-XXXXXX");
    assert(mkdtemp(fixture.root));
    join(fixture.bin, sizeof(fixture.bin), "bin");
    join(fixture.api, sizeof(fixture.api), "api.sock");
    join(fixture.log, sizeof(fixture.log), "ssh.log");
    join(fixture.out, sizeof(fixture.out), "case.out");
    join(fixture.leftover, sizeof(fixture.leftover), "leftover");
    join(fixture.profile.dir, sizeof(fixture.profile.dir), "profile");
    snprintf(fixture.profile.name, sizeof(fixture.profile.name), "waits");
    assert(profile_path(&fixture.profile, "docker.sock", fixture.docker_socket,
                        sizeof(fixture.docker_socket)));
    assert(mkdir(fixture.bin, 0700) == 0 && mkdir(fixture.profile.dir, 0700) == 0);
    char fake[sizeof(fixture.bin) + 8];
    snprintf(fake, sizeof(fake), "%s/ssh", fixture.bin);
    assert(symlink(self, fake) == 0);
    char path[sizeof(fixture.bin) + 32];
    snprintf(path, sizeof(path), "%s:/usr/bin:/bin", fixture.bin);
    assert(setenv("PATH", path, 1) == 0 && setenv(ENV_LOG, fixture.log, 1) == 0 &&
           setenv(ENV_API, fixture.api, 1) == 0 &&
           setenv("HAMN_TEST_SUPERVISOR_GRACE_MS", "50", 1) == 0);

    run_case("ready_when_every_probe_answers", ready_when_every_probe_answers,
             API_ANSWERS, 2000 + WATCHDOG_EXTRA_MS);
    run_case("a_failing_probe_is_not_ready", a_failing_probe_is_not_ready,
             API_ANSWERS, 1000 + WATCHDOG_EXTRA_MS);
    run_case("hung_agent_probe_ends_at_the_budget",
             hung_agent_probe_ends_at_the_budget, API_ANSWERS,
             2000 + WATCHDOG_EXTRA_MS);
    run_case("hung_docker_probe_ends_at_the_budget",
             hung_docker_probe_ends_at_the_budget, API_ANSWERS,
             2000 + WATCHDOG_EXTRA_MS);
    run_case("hung_containerd_probe_ends_at_the_budget",
             hung_containerd_probe_ends_at_the_budget, API_ANSWERS,
             2000 + WATCHDOG_EXTRA_MS);
    run_case("silent_docker_api_ends_at_the_budget",
             silent_docker_api_ends_at_the_budget, API_SILENT,
             1000 + WATCHDOG_EXTRA_MS);
    run_case("slow_guest_is_ready_within_the_running_vm_budget",
             slow_guest_is_ready_within_the_running_vm_budget, API_ANSWERS,
             (1 + GUEST_DEPLOYMENT_RUNNING_READY_TIMEOUT_SEC) * 1000LL +
             WATCHDOG_EXTRA_MS);
    run_case("cancellation_ends_the_readiness_wait",
             cancellation_ends_the_readiness_wait, API_ANSWERS,
             2000 + WATCHDOG_EXTRA_MS);
    run_case("hang_in_cleanup_after_cancellation_ends_at_the_budget",
             hang_in_cleanup_after_cancellation_ends_at_the_budget,
             API_ANSWERS, READY_MS + WATCHDOG_EXTRA_MS);
    run_case("reconcile_without_a_leftover_configures_docker_only",
             reconcile_without_a_leftover_configures_docker_only, API_ANSWERS,
             READY_MS + WATCHDOG_EXTRA_MS);
    run_case("reconcile_recovers_a_leftover_and_applies_the_whole_runtime",
             reconcile_recovers_a_leftover_and_applies_the_whole_runtime,
             API_ANSWERS, READY_MS + VERIFY_MS + WATCHDOG_EXTRA_MS);
    run_case("reconcile_begins_nothing_when_recovery_fails",
             reconcile_begins_nothing_when_recovery_fails, API_ANSWERS,
             WATCHDOG_EXTRA_MS);
    run_case("cancelled_recovery_keeps_its_result_in_reconcile",
             cancelled_recovery_keeps_its_result_in_reconcile, API_ANSWERS,
             READY_MS + WATCHDOG_EXTRA_MS);
    run_case("hung_image_check_ends_at_its_bound",
             hung_image_check_ends_at_its_bound, API_ANSWERS,
             VERIFY_MS + WATCHDOG_EXTRA_MS);
    run_case("image_check_output_stays_visible",
             image_check_output_stays_visible, API_ANSWERS,
             VERIFY_MS + WATCHDOG_EXTRA_MS);
    run_case("cancelled_lease_wait_returns_at_once",
             cancelled_lease_wait_returns_at_once, API_ANSWERS, 5000);
    run_case("cancellation_ends_the_lease_wait",
             cancellation_ends_the_lease_wait, API_ANSWERS, 5000);
    run_case("lease_wait_without_a_lease_times_out",
             lease_wait_without_a_lease_times_out, API_ANSWERS, 5000);

    char command[sizeof(fixture.root) + 16];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", fixture.root);
    int removed = system(command) == 0;
    if (failures) {
        fprintf(stderr, "FAIL: %d guest wait case(s)\n", failures);
        return 1;
    }
    assert(removed);
    puts("PASS: guest readiness probes, the image check and the DHCP lease "
         "wait end at their deadline or on cancellation");
    return 0;
}
