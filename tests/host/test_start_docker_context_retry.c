#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "cli.h"
#include "core/profile.h"
#include "core/state.h"

/* Test-only seam from host/cmd/cmd_start.c. */
int hamn_test_start_retry_running_docker_context(
    const struct profile *profile, struct vm_state *state);
int hamn_test_start_ensure_signed_guest_image(char *image, size_t capacity,
                                              int *updated);
int cmd_start(int argc, char **argv);

static int write_text(const char *path, const char *text, mode_t mode)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, mode);
    if (fd < 0)
        return -1;
    size_t length = strlen(text);
    size_t written = 0;
    while (written < length) {
        ssize_t count = write(fd, text + written, length - written);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            close(fd);
            return -1;
        }
        written += (size_t)count;
    }
    return close(fd);
}

static int read_text(const char *path, char *text, size_t capacity)
{
    if (capacity == 0)
        return -1;
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    size_t length = 0;
    for (;;) {
        ssize_t count = read(fd, text + length, capacity - 1 - length);
        if (count > 0) {
            length += (size_t)count;
            if (length == capacity - 1) {
                char extra;
                if (read(fd, &extra, 1) != 0) {
                    close(fd);
                    return -1;
                }
                break;
            }
            continue;
        }
        if (count == 0)
            break;
        if (errno != EINTR) {
            close(fd);
            return -1;
        }
    }
    if (close(fd) != 0)
        return -1;
    text[length] = '\0';
    return 0;
}

static int join_path(char *output, size_t capacity, const char *base,
                     const char *name)
{
    int length = snprintf(output, capacity, "%s/%s", base, name);
    return length >= 0 && (size_t)length < capacity ? 0 : -1;
}

static int require_text(const char *path, const char *expected)
{
    char actual[PATH_MAX];
    if (read_text(path, actual, sizeof(actual)) != 0 ||
        strcmp(actual, expected) != 0) {
        fprintf(stderr, "unexpected %s\n", path);
        return -1;
    }
    return 0;
}

static int require_state(const struct profile *profile, const char *previous)
{
    struct vm_state loaded;
    if (state_load(profile, &loaded) != 0 || strcmp(loaded.state, "running") != 0 ||
        strcmp(loaded.prev_docker_context, previous) != 0) {
        fprintf(stderr, "Docker context state was not persisted as expected\n");
        return -1;
    }
    return 0;
}

static void cleanup_path(const char *path)
{
    (void)unlink(path);
}

int main(void)
{
    static const char image_hash[] =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    static const char docker_script[] =
        "#!/bin/sh\n"
        "set -eu\n"
        "state=${HAMN_TEST_DOCKER_STATE:?}\n"
        "case \"${1:-}:${2:-}\" in\n"
        "context:show) /bin/cat \"$state/current\" ;;\n"
        "context:inspect)\n"
        "  [ \"${3:-}\" = hamn ] || exit 90\n"
        "  [ -f \"$state/endpoint\" ] || exit 1\n"
        "  /bin/cat \"$state/endpoint\"\n"
        "  ;;\n"
        "context:create)\n"
        "  [ \"${3:-}\" = hamn ] && [ \"${4:-}\" = --docker ] || exit 91\n"
        "  [ ! -e \"$state/create-fail\" ] || exit 2\n"
        "  case \"${5:-}\" in host=*) ;; *) exit 92 ;; esac\n"
        "  printf '%s\\n' \"${5#host=}\" >\"$state/endpoint\"\n"
        "  ;;\n"
        "context:use)\n"
        "  [ \"${3:-}\" = hamn ] || exit 93\n"
        "  [ ! -e \"$state/use-fail\" ] || exit 3\n"
        "  printf '%s\\n' hamn >\"$state/current\"\n"
        "  ;;\n"
        "*) exit 94 ;;\n"
        "esac\n";
    static const char bootstrap_script[] =
        "#!/bin/sh\n"
        "set -eu\n"
        "state=${HAMN_TEST_BOOTSTRAP_STATE:?}\n"
        "case \"${1:-}\" in\n"
        "update)\n"
        "  printf 'update\\n' >>\"$state\"\n"
        "  [ \"${HAMN_TEST_BOOTSTRAP_FAIL:-0}\" = 0 ] || exit 77\n"
        "  cache=\"$HOME/.hamn/cache\"\n"
        "  /bin/mkdir -p \"$cache\"\n"
        "  printf 'guest fixture\\n' >\"$cache/hamn-guest-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.img\"\n"
        "  printf '%s\\n' 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef >\"$cache/hamn-guest-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.img.verified\"\n"
        "  printf '%s\\n' '{\"schemaVersion\":1,\"file\":\"hamn-guest-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.img\",\"sha256\":\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"}' >\"$cache/guest-image.json\"\n"
        "  ;;\n"
        "start) printf 'start\\n' >>\"$state\" ;;\n"
        "*) exit 90 ;;\n"
        "esac\n";
    char root[] = "/tmp/hamn-start-context.XXXXXX";
    char bin[PATH_MAX], no_docker_bin[PATH_MAX], fake_docker[PATH_MAX];
    char docker_state[PATH_MAX], bootstrap[PATH_MAX], bootstrap_state[PATH_MAX];
    char bootstrap_home[PATH_MAX], bootstrap_cache[PATH_MAX], reexec_home[PATH_MAX];
    char image[PATH_MAX];
    char profile_dir[PATH_MAX], path[PATH_MAX];
    char expected_endpoint[PATH_MAX + 8], expected_endpoint_line[PATH_MAX + 9];
    int rc = 1;

    if (!mkdtemp(root) ||
        join_path(bin, sizeof(bin), root, "bin") != 0 ||
        join_path(no_docker_bin, sizeof(no_docker_bin), root, "no-docker-bin") != 0 ||
        join_path(fake_docker, sizeof(fake_docker), bin, "docker") != 0 ||
        join_path(docker_state, sizeof(docker_state), root, "docker-state") != 0 ||
        join_path(bootstrap, sizeof(bootstrap), root, "hamn") != 0 ||
        join_path(bootstrap_state, sizeof(bootstrap_state), root, "bootstrap-state") != 0 ||
        join_path(bootstrap_home, sizeof(bootstrap_home), root, "bootstrap-home") != 0 ||
        join_path(bootstrap_cache, sizeof(bootstrap_cache), bootstrap_home,
                  ".hamn/cache") != 0 ||
        join_path(reexec_home, sizeof(reexec_home), root, "reexec-home") != 0 ||
        join_path(profile_dir, sizeof(profile_dir), root, "profile") != 0 ||
        mkdir(bin, 0700) != 0 || mkdir(no_docker_bin, 0700) != 0 ||
        mkdir(docker_state, 0700) != 0 ||
        mkdir(bootstrap_home, 0700) != 0 ||
        mkdir(reexec_home, 0700) != 0 ||
        mkdir(profile_dir, 0700) != 0 ||
        write_text(fake_docker, docker_script, 0700) != 0 ||
        write_text(bootstrap, bootstrap_script, 0700) != 0 ||
        write_text(bootstrap_state, "", 0600) != 0 ||
        join_path(path, sizeof(path), docker_state, "current") != 0 ||
        write_text(path, "foreign\n", 0600) != 0 ||
        setenv("HAMN_TEST_DOCKER_STATE", docker_state, 1) != 0 ||
        setenv("PATH", no_docker_bin, 1) != 0) {
        perror("cannot prepare Docker context retry test");
        goto out;
    }

    /* First start downloads only a signed selected image through `hamn
     * update`, then immediately retries image selection.  A bad selection
     * must not invoke the updater, and update failure must leave no selection
     * behind. */
    if (setenv("HOME", bootstrap_home, 1) != 0 ||
        setenv("HAMN_TEST_BOOTSTRAP_STATE", bootstrap_state, 1) != 0 ||
        unsetenv("HAMN_TEST_BOOTSTRAP_FAIL") != 0) {
        perror("cannot configure signed-image bootstrap test");
        goto out;
    }
    cli_set_invocation_path(bootstrap);
    int updated = 0;
    if (snprintf(image, sizeof(image), "%s/hamn-guest-%s.img", bootstrap_cache,
                 image_hash) >= (int)sizeof(image) ||
        hamn_test_start_ensure_signed_guest_image(path, sizeof(path),
                                                  &updated) != 0 ||
        !updated || strcmp(path, image) != 0 ||
        require_text(bootstrap_state, "update\n") != 0) {
        fprintf(stderr, "missing signed guest image was not bootstrapped\n");
        goto out;
    }
    char selection[PATH_MAX];
    if (join_path(selection, sizeof(selection), bootstrap_cache,
                  "guest-image.json") != 0 || unlink(selection) != 0 ||
        join_path(path, sizeof(path), bootstrap_cache,
                  "hamn-guest-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.img") != 0 ||
        unlink(path) != 0 ||
        strlcat(path, ".verified", sizeof(path)) >= sizeof(path) ||
        unlink(path) != 0 || write_text(bootstrap_state, "", 0600) != 0 ||
        setenv("HAMN_TEST_BOOTSTRAP_FAIL", "1", 1) != 0 ||
        hamn_test_start_ensure_signed_guest_image(path, sizeof(path),
                                                  &updated) == 0 ||
        updated ||
        require_text(bootstrap_state, "update\n") != 0 ||
        access(selection, F_OK) == 0) {
        fprintf(stderr, "failed signed-image bootstrap left mutable selection state\n");
        goto out;
    }
    if (unsetenv("HAMN_TEST_BOOTSTRAP_FAIL") != 0 ||
        write_text(selection, "{}\n", 0600) != 0 ||
        write_text(bootstrap_state, "", 0600) != 0 ||
        hamn_test_start_ensure_signed_guest_image(path, sizeof(path),
                                                  &updated) == 0 ||
        updated ||
        require_text(bootstrap_state, "") != 0) {
        fprintf(stderr, "invalid image selection invoked the signed updater\n");
        goto out;
    }

    /* Once update has selected an image, start must release its locks and
     * exec the newly installed Hamn before it touches a VM disk. */
    if (setenv("HOME", reexec_home, 1) != 0 ||
        write_text(bootstrap_state, "", 0600) != 0) {
        perror("cannot prepare start re-exec test");
        goto out;
    }
    pid_t reexec_pid = fork();
    if (reexec_pid < 0) {
        perror("cannot fork start re-exec test");
        goto out;
    }
    if (reexec_pid == 0) {
        char *start_argv[] = { "start", "--profile", "reexec", NULL };
        _exit(cmd_start(3, start_argv));
    }
    int reexec_status;
    if (waitpid(reexec_pid, &reexec_status, 0) != reexec_pid ||
        !WIFEXITED(reexec_status) || WEXITSTATUS(reexec_status) != 0 ||
        require_text(bootstrap_state, "update\nstart\n") != 0) {
        fprintf(stderr, "signed-image bootstrap did not restart Hamn start\n");
        goto out;
    }
    if (join_path(path, sizeof(path), reexec_home, ".hamn/reexec/vmrun.pid") != 0 ||
        access(path, F_OK) == 0) {
        fprintf(stderr, "old Hamn process reached VM startup after signed update\n");
        goto out;
    }

    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.name, sizeof(profile.name), "default");
    snprintf(profile.dir, sizeof(profile.dir), "%s", profile_dir);
    struct vm_state state;
    memset(&state, 0, sizeof(state));
    snprintf(state.state, sizeof(state.state), "running");
    snprintf(state.ip, sizeof(state.ip), "192.0.2.10");

    /* The running VM is not rolled back while Docker CLI is absent. When the
     * CLI appears later, the next start retry creates and activates context. */
    if (hamn_test_start_retry_running_docker_context(&profile, &state) != 0 ||
        join_path(path, sizeof(path), docker_state, "endpoint") != 0 ||
        access(path, F_OK) == 0 || setenv("PATH", bin, 1) != 0) {
        fprintf(stderr, "missing Docker CLI did not remain retryable\n");
        goto out;
    }

    /* The first create attempt can fail after a VM is already healthy. It
     * must leave that VM's durable state alone and succeed on a later retry. */
    if (state_save(&profile, &state) != 0 ||
        join_path(path, sizeof(path), docker_state, "create-fail") != 0 ||
        write_text(path, "fail\n", 0600) != 0 ||
        join_path(path, sizeof(path), profile_dir, "vmrun.pid") != 0 ||
        write_text(path, "still-running\n", 0600) != 0 ||
        hamn_test_start_retry_running_docker_context(&profile, &state) == 0 ||
        join_path(path, sizeof(path), docker_state, "current") != 0 ||
        require_text(path, "foreign\n") != 0 || require_state(&profile, "") != 0 ||
        join_path(path, sizeof(path), profile_dir, "vmrun.pid") != 0 ||
        require_text(path, "still-running\n") != 0 ||
        join_path(path, sizeof(path), docker_state, "create-fail") != 0) {
        fprintf(stderr, "failed initial context creation changed running VM ownership\n");
        goto out;
    }
    cleanup_path(path);

    if (snprintf(expected_endpoint, sizeof(expected_endpoint), "unix://%s/docker.sock",
                 profile_dir) >= (int)sizeof(expected_endpoint) ||
        snprintf(expected_endpoint_line, sizeof(expected_endpoint_line), "%s\n",
                 expected_endpoint) >= (int)sizeof(expected_endpoint_line) ||
        hamn_test_start_retry_running_docker_context(&profile, &state) != 0 ||
        join_path(path, sizeof(path), docker_state, "current") != 0 ||
        require_text(path, "hamn\n") != 0 ||
        join_path(path, sizeof(path), docker_state, "endpoint") != 0 ||
        require_text(path, expected_endpoint_line) != 0 ||
        require_state(&profile, "foreign") != 0) {
        fprintf(stderr, "initial Docker context activation failed\n");
        goto out;
    }

    /* A user-selected foreign context is reactivated to this running profile. */
    if (join_path(path, sizeof(path), docker_state, "current") != 0 ||
        write_text(path, "outside\n", 0600) != 0 ||
        hamn_test_start_retry_running_docker_context(&profile, &state) != 0 ||
        require_text(path, "hamn\n") != 0 || require_state(&profile, "outside") != 0) {
        fprintf(stderr, "running profile did not reactivate its Docker context\n");
        goto out;
    }

    /* A context-use failure returns an error, preserves running VM state, and
     * can be retried after the host Docker CLI recovers. */
    if (write_text(path, "foreign\n", 0600) != 0 ||
        join_path(path, sizeof(path), docker_state, "use-fail") != 0 ||
        write_text(path, "fail\n", 0600) != 0 ||
        join_path(path, sizeof(path), profile_dir, "vmrun.pid") != 0 ||
        write_text(path, "still-running\n", 0600) != 0 ||
        hamn_test_start_retry_running_docker_context(&profile, &state) == 0 ||
        join_path(path, sizeof(path), docker_state, "current") != 0 ||
        require_text(path, "foreign\n") != 0 || require_state(&profile, "") != 0 ||
        join_path(path, sizeof(path), profile_dir, "vmrun.pid") != 0 ||
        require_text(path, "still-running\n") != 0) {
        fprintf(stderr, "failed context retry changed running VM ownership\n");
        goto out;
    }
    if (join_path(path, sizeof(path), docker_state, "use-fail") != 0) {
        goto out;
    }
    cleanup_path(path);
    if (hamn_test_start_retry_running_docker_context(&profile, &state) != 0 ||
        join_path(path, sizeof(path), docker_state, "current") != 0 ||
        require_text(path, "hamn\n") != 0 || require_state(&profile, "foreign") != 0) {
        fprintf(stderr, "Docker context did not recover on retry\n");
        goto out;
    }
    rc = 0;

out:
    if (join_path(path, sizeof(path), bootstrap_cache, "guest-image.json") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), bootstrap_cache,
                  "hamn-guest-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.img.verified") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), bootstrap_cache,
                  "hamn-guest-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.img") == 0)
        cleanup_path(path);
    (void)rmdir(bootstrap_cache);
    if (join_path(path, sizeof(path), bootstrap_home, ".hamn") == 0)
        (void)rmdir(path);
    cleanup_path(bootstrap_state);
    cleanup_path(bootstrap);
    (void)rmdir(bootstrap_home);
    if (join_path(path, sizeof(path), reexec_home, ".hamn/reexec/config.yaml") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), reexec_home, ".hamn/reexec") == 0)
        (void)rmdir(path);
    if (join_path(path, sizeof(path), reexec_home, ".hamn/cache/guest-image.json") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), reexec_home,
                  ".hamn/cache/hamn-guest-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.img.verified") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), reexec_home,
                  ".hamn/cache/hamn-guest-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.img") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), reexec_home, ".hamn/cache") == 0)
        (void)rmdir(path);
    if (join_path(path, sizeof(path), reexec_home, ".hamn") == 0)
        (void)rmdir(path);
    (void)rmdir(reexec_home);
    if (join_path(path, sizeof(path), profile_dir, "state.json") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), profile_dir, "vmrun.pid") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), docker_state, "current") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), docker_state, "endpoint") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), docker_state, "use-fail") == 0)
        cleanup_path(path);
    if (join_path(path, sizeof(path), docker_state, "create-fail") == 0)
        cleanup_path(path);
    cleanup_path(fake_docker);
    (void)rmdir(docker_state);
    (void)rmdir(bin);
    (void)rmdir(no_docker_bin);
    (void)rmdir(profile_dir);
    (void)rmdir(root);
    if (rc == 0)
        puts("PASS: running Docker context is retried without VM rollback");
    return rc;
}
