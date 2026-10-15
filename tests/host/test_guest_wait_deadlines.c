/* The guest waits of a VM start end at their deadline or on cancellation:
 * every probe of guest_deployment_runtime_ready() runs with the readiness
 * budget that is left, the guest image check has its own bound, the client
 * of a guest writer, of the cloud-init wait and of a provisioning hook is
 * reaped once the guest's own limit for that command has passed, the vmrun
 * wait returns once vmrun is verified and ends at its deadline when vmrun's
 * control socket never answers or stops answering right after it reports
 * running, and the vmrun and DHCP lease waits end when the operation is
 * cancelled. Each case runs in a child that this test kills when it outlives
 * its limit, so a wait without a deadline fails its case instead of hanging
 * the test.
 *
 * `ssh` is this test executable, first in PATH. It answers control requests
 * (-O) with success, links a forwarded Docker socket to a fake Docker API,
 * and runs no remote command: the command succeeds, fails, is slow or hangs
 * as the case's environment asks. The fake Docker API answers _ping, or
 * accepts and never answers; silent, it also stands in for a stuck vmrun
 * control socket. The Makefile builds guest_deployment.c, remote_mutation.c,
 * provision.c and cmd_start.c into this test with guest limits and budgets
 * of seconds and no client margin. */
#if !defined(GUEST_DEPLOYMENT_READY_TIMEOUT_SEC) || \
    !defined(GUEST_IMAGE_VERIFY_TIMEOUT_SEC) || \
    !defined(GUEST_CLOUD_INIT_TIMEOUT_SEC) || \
    !defined(REMOTE_MUTATION_SLACK_SEC) || !defined(SSH_REMOTE_MARGIN_SEC)
#error "build with the test budgets of guest_deployment.c (see Makefile)"
#endif
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <poll.h>
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
#include "core/provision.h"
#include "core/remote_mutation.h"
#include "util/dhcp_leases.h"
#include "util/fs.h"
#include "util/proc.h"

/* Test-only seam from host/cmd/cmd_start.c. */
int hamn_test_start_wait_vmrun_running(const struct profile *p,
                                       const char *ctl, int timeout_sec);

#define READY_MS (GUEST_DEPLOYMENT_READY_TIMEOUT_SEC * 1000LL)
#define VERIFY_MS (GUEST_IMAGE_VERIFY_TIMEOUT_SEC * 1000LL)
/* A guest command bounded by `timeout --kill-after=5s <limit>` ends within
 * limit + 5 s; its client is reaped after that and the client margin. */
#define CLIENT_MS(limit_sec) \
    (((limit_sec) + 5LL + SSH_REMOTE_MARGIN_SEC) * 1000LL)
/* The guest's limit for a writer that waits 1 s for its lock and runs 1 s. */
#define WRITER_LIMIT_SEC (1 + 1 + REMOTE_MUTATION_SLACK_SEC)
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
/* A line the hanging command prints before it hangs. */
#define ENV_SAY "HAMN_TEST_FAKE_SSH_SAY"

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
        const char *say = getenv(ENV_SAY);
        if (say && (puts(say) == EOF || fflush(stdout) != 0))
            return 254;
        const char *owner = getenv(ENV_CANCEL);
        if (owner && kill((pid_t)atoi(owner), SIGTERM) != 0)
            return 254;
        for (;;)
            pause();
    }
    if (fake_matches(ENV_FAIL, command))
        return 3;
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
    char ctl[128];
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

/* The place of the first fake ssh invocation whose command contains needle
 * among all invocations, or -1. */
static int ssh_call_index(const char *needle)
{
    char *text = read_file(fixture.log);
    int index = 0, found = -1;
    for (char *line = text; *line && found < 0; index++) {
        char *end = strchr(line, '\n');
        assert(end);
        *end = '\0';
        char *command = strchr(line, '\t');
        assert(command);
        if (strstr(command + 1, needle))
            found = index;
        line = end + 1;
    }
    free(text);
    return found;
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

/* ---- guest writers --------------------------------------------------- */

#define WRITER "hamn-test-writer"
/* The fence publishes the writer's token; the barrier takes the lock. */
#define FENCE ": > \"$d/$1\""
#define BARRIER "flock --wait 1 /run/hamn-deployment.lock true"

/* A writer's client that outlives the guest's limit is reaped there, and the
 * writer's outcome is unknown: its token is fenced, then the barrier waits
 * for a writer that may hold the lock, in that order. */
static void hung_writer(char *reason, size_t capacity, int *truncated)
{
    setenv(ENV_HANG, WRITER, 1);
    const char *command[] = { "sudo", WRITER, NULL };
    long long bound = CLIENT_MS(WRITER_LIMIT_SEC);
    long long started = now_ms();
    int rc = remote_mutation_run(&fixture.profile, GUEST_IP, 1, 1, command,
                                 reason, capacity, truncated);
    long long elapsed = now_ms() - started;
    assert(rc == PROC_RUN_TIMEOUT);
    assert(elapsed >= bound && elapsed < bound + 2000);
    assert(ssh_calls(WRITER) == 1 && ssh_calls(FENCE) == 1 &&
           ssh_calls(BARRIER) == 1);
    assert(ssh_call_index(WRITER) < ssh_call_index(FENCE));
    assert(ssh_call_index(FENCE) < ssh_call_index(BARRIER));
    assert(!remote_mutation_cleanup_pending());
    ssh_clients_reaped();
}

static void hung_writer_is_reaped_then_fenced(void)
{
    hung_writer(NULL, 0, NULL);
}

/* What the writer said before it hung stays the failure's reason. */
static void hung_capturing_writer_keeps_its_reason(void)
{
    setenv(ENV_SAY, "recovering transaction 1 of 2", 1);
    char reason[64] = "";
    int truncated = -1;
    hung_writer(reason, sizeof(reason), &truncated);
    assert(strcmp(reason, "recovering transaction 1 of 2") == 0);
    assert(truncated == 0);
}

/* ---- cloud-init wait and provisioning hooks -------------------------- */

static void hung_cloud_init_wait_ends_at_its_bound(void)
{
    setenv(ENV_HANG, "cloud-init status --wait", 1);
    setenv(ENV_SAY, "status: running", 1);
    struct vm_state state;
    memset(&state, 0, sizeof(state));
    snprintf(state.ip, sizeof(state.ip), "%s", GUEST_IP);
    long long bound = CLIENT_MS(GUEST_CLOUD_INIT_TIMEOUT_SEC);
    long long started = now_ms();
    assert(guest_deployment_refresh_locked(&fixture.profile, &state) == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= bound && elapsed < bound + 2000);
    assert(strstr(log_last_error(), "cloud-init did not finish within"));
    assert(ssh_calls("cloud-init status --wait") == 1);
    assert(ssh_calls("guest-deployment-transaction begin") == 0);
    /* Its report still reaches the operation's output. */
    fflush(NULL);
    char *output = read_file(fixture.out);
    assert(strstr(output, "status: running"));
    free(output);
    ssh_clients_reaped();
}

/* A hook's client is reaped after the hook's timeoutSeconds have passed in
 * the guest, and the hook fails as a timed-out hook does. */
static void hung_provision_hook_ends_at_its_bound(void)
{
    struct profile_hook *hook = &fixture.profile.hooks[0];
    fixture.profile.hook_count = 1;
    snprintf(hook->stage, sizeof(hook->stage), "system");
    snprintf(hook->command, sizeof(hook->command), "hamn-test-hook");
    hook->timeout_seconds = 1;
    setenv(ENV_HANG, "hamn-test-hook", 1);
    long long bound = CLIENT_MS(1);
    long long started = now_ms();
    assert(provision_run_stage(&fixture.profile, GUEST_IP, "system") == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= bound && elapsed < bound + 2000);
    assert(strstr(log_last_error(), "hook 0 (system) failed with exit 124"));
    char path[PATH_MAX];
    assert(profile_path(&fixture.profile, "logs/provision-system-0.log", path,
                        sizeof(path)));
    char *text = read_file(path);
    assert(strstr(text, "result=failure") && strstr(text, "exitCode=124"));
    free(text);
    assert(ssh_calls("hamn-test-hook") == 1);
    ssh_clients_reaped();
}

/* ---- vmrun wait ------------------------------------------------------ */

static void cancelled_vmrun_wait_returns_at_once(void)
{
    assert(proc_cancel_install() == 0);
    assert(raise(SIGTERM) == 0 && proc_cancelled());
    long long started = now_ms();
    errno = 0;
    assert(hamn_test_start_wait_vmrun_running(&fixture.profile, fixture.ctl,
                                              20) == -1);
    assert(errno == ECANCELED);
    assert(now_ms() - started < 500);
}

static void cancellation_ends_the_vmrun_wait(void)
{
    assert(proc_cancel_install() == 0);
    pid_t owner = getpid();
    pid_t canceller = fork();
    assert(canceller >= 0);
    if (canceller == 0) {
        sleep_ms(300); /* while the wait polls or sleeps */
        _exit(kill(owner, SIGTERM) == 0 ? 0 : 1);
    }
    long long started = now_ms();
    errno = 0;
    int rc = hamn_test_start_wait_vmrun_running(&fixture.profile, fixture.ctl,
                                                20);
    int saved = errno;
    long long elapsed = now_ms() - started;
    int status = 0;
    while (waitpid(canceller, &status, 0) < 0 && errno == EINTR) {}
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(rc == -1 && saved == ECANCELED);
    assert(elapsed < 1500); /* of a 20-second wait */
}

static void vmrun_wait_without_a_vm_times_out(void)
{
    long long started = now_ms();
    assert(hamn_test_start_wait_vmrun_running(&fixture.profile, fixture.ctl,
                                              1) == -1);
    long long elapsed = now_ms() - started;
    assert(elapsed >= 1000 && elapsed < 3000);
    assert(!proc_cancelled());
}

/* A vmrun whose control socket accepts and never answers, as when its main
 * queue is stuck: the silent fake Docker API stands in for that socket. The
 * wait ends at its deadline, not after a number of polls that each wait out
 * a status query and a sleep. */
static void silent_vmrun_socket_ends_the_wait_at_its_deadline(void)
{
    long long started = now_ms();
    errno = 0;
    int rc = hamn_test_start_wait_vmrun_running(&fixture.profile, fixture.api,
                                                2);
    int saved = errno;
    long long elapsed = now_ms() - started;
    assert(rc == -1);
    assert(elapsed >= 2000 && elapsed < 2000 + MARGIN_MS);
    assert(saved == ETIMEDOUT && !proc_cancelled());
}

/* A fake vmrun on the profile's control socket. It reports running with its
 * own pid and start token, as vmrun does: at once on every connection
 * (VMRUN_RUNNING), or only on the second one, RUNNING_LATE_MS after it
 * accepted the first or at once if that time has passed, holding every other
 * connection open unanswered (VMRUN_RUNNING_LATE). It writes 'R' to its
 * events for each reply it has sent and 'H' for each connection it holds,
 * and exits when the case closes life. */
enum vmrun_mode { VMRUN_RUNNING, VMRUN_RUNNING_LATE };

/* A 1-second wait queries at once and, after that query's 300 ms and a sleep
 * of 500 ms, again about 800 ms in, until its deadline at 1000 ms. */
#define RUNNING_LATE_MS 940
/* Verifying the process after "running" with a query of its own 300 ms ends
 * the wait at least 1240 ms in; one that gets what is left ends it at 1000. */
#define RUNNING_LATE_SLACK_MS 200

struct vmrun_fake {
    pid_t pid;
    int life;   /* the case's end; closing it ends the fake */
    int events; /* the read end of the fake's events */
};

static void vmrun_fake_serve(enum vmrun_mode mode, int listener, int life,
                             int events)
{
    signal(SIGPIPE, SIG_IGN);
    uint64_t sec = 0, usec = 0;
    if (proc_start_identity(getpid(), &sec, &usec) != 0)
        _exit(1);
    char reply[128];
    int length = snprintf(reply, sizeof(reply),
                          "{\"state\":\"running\",\"pid\":%d,\"start_sec\":%"
                          PRIu64 ",\"start_usec\":%" PRIu64 "}\n",
                          (int)getpid(), sec, usec);
    if (length <= 0 || (size_t)length >= sizeof(reply))
        _exit(1);
    int accepted = 0, pending = -1;
    long long reply_at = 0;
    for (;;) {
        int timeout = -1;
        if (pending >= 0) {
            long long left = reply_at - now_ms();
            timeout = left > 0 ? (int)left : 0;
        }
        struct pollfd fds[2] = {
            { .fd = listener, .events = POLLIN },
            { .fd = life, .events = POLLIN },
        };
        if (poll(fds, 2, timeout) < 0) {
            if (errno == EINTR)
                continue;
            _exit(1);
        }
        if (fds[1].revents)
            _exit(0);
        if (pending >= 0 && now_ms() >= reply_at) {
            if (proc_write_all(pending, reply, (size_t)length) == 0 &&
                write(events, "R", 1) != 1)
                _exit(1);
            pending = -1; /* held open and never answered again */
        }
        if (!(fds[0].revents & POLLIN))
            continue;
        int connection = accept(listener, NULL, NULL);
        if (connection < 0) {
            if (errno == EINTR || errno == ECONNABORTED)
                continue;
            _exit(1);
        }
        accepted++;
        if (mode == VMRUN_RUNNING) {
            /* Read the request first: closing before it arrived would make
             * the client retry as with an old vmrun (ctlsock.h). */
            char request[512];
            if (read(connection, request, sizeof(request)) <= 0 ||
                proc_write_all(connection, reply, (size_t)length) != 0 ||
                write(events, "R", 1) != 1)
                _exit(1);
            close(connection);
            continue;
        }
        if (accepted == 1)
            reply_at = now_ms() + RUNNING_LATE_MS;
        if (accepted == 2)
            pending = connection;
        else if (write(events, "H", 1) != 1)
            _exit(1);
    }
}

static void vmrun_fake_start(const char *path, enum vmrun_mode mode,
                             struct vmrun_fake *vmrun)
{
    (void)unlink(path);
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    assert(strlen(path) < sizeof(address.sun_path));
    snprintf(address.sun_path, sizeof(address.sun_path), "%s", path);
    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    assert(listener >= 0);
    assert(bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0);
    assert(listen(listener, 16) == 0);
    int life[2], events[2];
    assert(pipe(life) == 0 && pipe(events) == 0);
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        close(life[1]);
        close(events[0]);
        vmrun_fake_serve(mode, listener, life[0], events[1]);
    }
    close(listener);
    close(life[0]);
    close(events[1]);
    vmrun->pid = pid;
    vmrun->life = life[1];
    vmrun->events = events[0];
}

/* Records the fake as the profile's vmrun, as vmrun itself does. */
static void vmrun_fake_record(const struct vmrun_fake *vmrun)
{
    uint64_t sec = 0, usec = 0;
    unsigned char uuid[16];
    assert(proc_start_identity(vmrun->pid, &sec, &usec) == 0);
    assert(proc_executable_identity(vmrun->pid, uuid) == 0);
    char hex[33], text[160], path[PATH_MAX];
    proc_executable_uuid_format(uuid, hex);
    int length = snprintf(text, sizeof(text), "%d %" PRIu64 " %" PRIu64
                          " %s\n", (int)vmrun->pid, sec, usec, hex);
    assert(length > 0 && (size_t)length < sizeof(text));
    assert(profile_path(&fixture.profile, "vmrun.identity", path,
                        sizeof(path)));
    assert(fs_write_file_atomic(path, text, (size_t)length, 0600) == 0);
    length = snprintf(text, sizeof(text), "%d\n", (int)vmrun->pid);
    assert(profile_path(&fixture.profile, "vmrun.pid", path, sizeof(path)));
    assert(fs_write_file_atomic(path, text, (size_t)length, 0600) == 0);
}

/* What the fake did: its first expected events, waiting at most 2 s for
 * them, then any others until it has exited. */
static void vmrun_fake_stop(struct vmrun_fake *vmrun, size_t expected,
                            char *seen, size_t capacity)
{
    assert(expected < capacity);
    size_t length = 0;
    long long deadline = now_ms() + 2000;
    while (length < expected) {
        long long left = deadline - now_ms();
        struct pollfd fd = { .fd = vmrun->events, .events = POLLIN };
        int ready = poll(&fd, 1, left > 0 ? (int)left : 0);
        if (ready < 0 && errno == EINTR)
            continue;
        if (ready <= 0)
            break;
        ssize_t n = read(vmrun->events, seen + length, expected - length);
        if (n <= 0)
            break;
        length += (size_t)n;
    }
    close(vmrun->life);
    while (waitpid(vmrun->pid, NULL, 0) < 0 && errno == EINTR) {}
    for (ssize_t n; length < capacity - 1 &&
         (n = read(vmrun->events, seen + length,
                   capacity - 1 - length)) > 0;)
        length += (size_t)n;
    seen[length] = '\0';
    close(vmrun->events);
}

static void remove_profile_file(const char *name)
{
    char path[PATH_MAX];
    assert(profile_path(&fixture.profile, name, path, sizeof(path)));
    assert(unlink(path) == 0 || errno == ENOENT);
}

/* A vmrun that reports running and is the profile's recorded vmrun: the
 * wait verifies its process and returns at once. */
static void verified_vmrun_ends_the_wait_at_once(void)
{
    char ctl[PATH_MAX];
    assert(profile_path(&fixture.profile, "vmrun.sock", ctl, sizeof(ctl)));
    struct vmrun_fake vmrun;
    vmrun_fake_start(ctl, VMRUN_RUNNING, &vmrun);
    vmrun_fake_record(&vmrun);
    long long started = now_ms();
    int rc = hamn_test_start_wait_vmrun_running(&fixture.profile, ctl, 2);
    long long elapsed = now_ms() - started;
    char seen[16];
    vmrun_fake_stop(&vmrun, 2, seen, sizeof(seen));
    remove_profile_file("vmrun.sock");
    remove_profile_file("vmrun.identity");
    remove_profile_file("vmrun.pid");
    printf("wait ended after %lld ms; vmrun events %s\n", elapsed, seen);
    /* It answered the status query and the one that verifies it. */
    assert(strcmp(seen, "RR") == 0);
    assert(rc == 0 && elapsed < 1000);
}

/* A vmrun that reports running 60 ms before the wait's deadline and then
 * stops answering. Verifying its process queries the control socket again;
 * that query gets what is left of the wait, so the wait still ends at its
 * deadline. */
static void silent_vmrun_after_running_ends_the_wait_at_its_deadline(void)
{
    char ctl[PATH_MAX];
    assert(profile_path(&fixture.profile, "vmrun.sock", ctl, sizeof(ctl)));
    struct vmrun_fake vmrun;
    vmrun_fake_start(ctl, VMRUN_RUNNING_LATE, &vmrun);
    long long started = now_ms();
    errno = 0;
    int rc = hamn_test_start_wait_vmrun_running(&fixture.profile, ctl, 1);
    int saved = errno;
    long long elapsed = now_ms() - started;
    char seen[16];
    vmrun_fake_stop(&vmrun, 3, seen, sizeof(seen));
    remove_profile_file("vmrun.sock");
    printf("wait ended after %lld ms; vmrun events %s\n", elapsed, seen);
    /* It held the first status query, reported running on the second, and
     * held the one that verifies it. */
    assert(strcmp(seen, "HRH") == 0);
    assert(rc == -1 && saved == ETIMEDOUT && !proc_cancelled());
    assert(elapsed >= 1000 && elapsed < 1000 + RUNNING_LATE_SLACK_MS);
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
    join(fixture.ctl, sizeof(fixture.ctl), "vmrun-ctl.sock"); /* never bound */
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
    run_case("hung_image_check_ends_at_its_bound",
             hung_image_check_ends_at_its_bound, API_ANSWERS,
             VERIFY_MS + WATCHDOG_EXTRA_MS);
    run_case("image_check_output_stays_visible",
             image_check_output_stays_visible, API_ANSWERS,
             VERIFY_MS + WATCHDOG_EXTRA_MS);
    run_case("hung_writer_is_reaped_then_fenced",
             hung_writer_is_reaped_then_fenced, API_ANSWERS,
             CLIENT_MS(WRITER_LIMIT_SEC) + WATCHDOG_EXTRA_MS);
    run_case("hung_capturing_writer_keeps_its_reason",
             hung_capturing_writer_keeps_its_reason, API_ANSWERS,
             CLIENT_MS(WRITER_LIMIT_SEC) + WATCHDOG_EXTRA_MS);
    run_case("hung_cloud_init_wait_ends_at_its_bound",
             hung_cloud_init_wait_ends_at_its_bound, API_ANSWERS,
             CLIENT_MS(GUEST_CLOUD_INIT_TIMEOUT_SEC) + WATCHDOG_EXTRA_MS);
    run_case("hung_provision_hook_ends_at_its_bound",
             hung_provision_hook_ends_at_its_bound, API_ANSWERS,
             CLIENT_MS(1) + WATCHDOG_EXTRA_MS);
    run_case("cancelled_vmrun_wait_returns_at_once",
             cancelled_vmrun_wait_returns_at_once, API_ANSWERS, 5000);
    run_case("cancellation_ends_the_vmrun_wait",
             cancellation_ends_the_vmrun_wait, API_ANSWERS, 5000);
    run_case("vmrun_wait_without_a_vm_times_out",
             vmrun_wait_without_a_vm_times_out, API_ANSWERS, 5000);
    run_case("silent_vmrun_socket_ends_the_wait_at_its_deadline",
             silent_vmrun_socket_ends_the_wait_at_its_deadline, API_SILENT,
             2000 + WATCHDOG_EXTRA_MS);
    run_case("verified_vmrun_ends_the_wait_at_once",
             verified_vmrun_ends_the_wait_at_once, API_ANSWERS,
             2000 + WATCHDOG_EXTRA_MS);
    run_case("silent_vmrun_after_running_ends_the_wait_at_its_deadline",
             silent_vmrun_after_running_ends_the_wait_at_its_deadline,
             API_ANSWERS, 1000 + WATCHDOG_EXTRA_MS);
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
    puts("PASS: guest readiness probes, the image check, guest writers, the "
         "cloud-init wait, provisioning hooks and the vmrun wait end at their "
         "deadline, and the vmrun and DHCP lease waits on cancellation");
    return 0;
}
