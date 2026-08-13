#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "core/profile.h"
#include "core/state.h"

int hamn_test_kubernetes_start(struct profile *profile, struct vm_state *state);

static void fail(const char *message)
{
    fprintf(stderr, "FAIL: %s\n", message);
    exit(1);
}

static void write_file(const char *path, const char *text, mode_t mode)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, mode);
    if (fd < 0)
        fail("cannot create fixture");
    size_t length = strlen(text);
    if (write(fd, text, length) != (ssize_t)length || fchmod(fd, mode) != 0 ||
        close(fd) != 0)
        fail("cannot write fixture");
}

static void read_file(const char *path, char *output, size_t capacity)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        fail("cannot read fixture");
    ssize_t count = read(fd, output, capacity - 1);
    if (count < 0 || close(fd) != 0)
        fail("cannot read fixture");
    output[count] = '\0';
}

static void expect_contains(const char *text, const char *needle,
                            const char *message)
{
    if (!strstr(text, needle))
        fail(message);
}

static void expect_file(const char *path, const char *expected,
                        const char *message)
{
    char actual[4096];
    read_file(path, actual, sizeof(actual));
    if (strcmp(actual, expected) != 0)
        fail(message);
}

static void reset_fixture(const char *path)
{
    if (unlink(path) != 0 && errno != ENOENT)
        fail("cannot reset fixture");
}

int main(void)
{
    char work[] = "/tmp/hamn-kubernetes-transaction.XXXXXX";
    if (!mkdtemp(work))
        fail("cannot create workspace");

    char home[PATH_MAX], hamn[PATH_MAX], profile_dir[PATH_MAX], kube_dir[PATH_MAX];
    char bin[PATH_MAX], config[PATH_MAX], local[PATH_MAX], global[PATH_MAX];
    char marker_dir[PATH_MAX], marker[PATH_MAX], kubectl[PATH_MAX], ssh[PATH_MAX];
    char ssh_log[PATH_MAX], forwards[PATH_MAX], kube_port[PATH_MAX], path[PATH_MAX * 2];
    if (snprintf(home, sizeof(home), "%s/home", work) >= (int)sizeof(home) ||
        snprintf(hamn, sizeof(hamn), "%s/.hamn", home) >= (int)sizeof(hamn) ||
        snprintf(profile_dir, sizeof(profile_dir), "%s/default", hamn) >=
            (int)sizeof(profile_dir) ||
        snprintf(kube_dir, sizeof(kube_dir), "%s/.kube", home) >=
            (int)sizeof(kube_dir) ||
        snprintf(bin, sizeof(bin), "%s/bin", work) >= (int)sizeof(bin) ||
        snprintf(config, sizeof(config), "%s/config.yaml", profile_dir) >=
            (int)sizeof(config) ||
        snprintf(local, sizeof(local), "%s/kubeconfig", profile_dir) >=
            (int)sizeof(local) ||
        snprintf(global, sizeof(global), "%s/config", kube_dir) >=
            (int)sizeof(global) ||
        snprintf(marker_dir, sizeof(marker_dir), "%s/.kube-contexts", hamn) >=
            (int)sizeof(marker_dir) ||
        snprintf(marker, sizeof(marker), "%s/default", marker_dir) >=
            (int)sizeof(marker) ||
        snprintf(kubectl, sizeof(kubectl), "%s/kubectl", bin) >=
            (int)sizeof(kubectl) ||
        snprintf(ssh, sizeof(ssh), "%s/ssh", bin) >= (int)sizeof(ssh) ||
        snprintf(ssh_log, sizeof(ssh_log), "%s/ssh.log", work) >=
            (int)sizeof(ssh_log) ||
        snprintf(forwards, sizeof(forwards), "%s/forwards", work) >=
            (int)sizeof(forwards) ||
        snprintf(kube_port, sizeof(kube_port), "%s/kube-api-port", profile_dir) >=
            (int)sizeof(kube_port) ||
        snprintf(path, sizeof(path), "%s:/usr/bin:/bin", bin) >=
            (int)sizeof(path))
        fail("test path is too long");
    if (mkdir(home, 0700) != 0 || mkdir(kube_dir, 0700) != 0 ||
        mkdir(bin, 0700) != 0)
        fail("cannot create test directories");

    write_file(kubectl,
               "#!/bin/sh\n"
               "set -eu\n"
               "case \"$*\" in\n"
               "*'config view --raw -o json'*)\n"
               "  printf '%s\\n' '{\"contexts\":[{\"name\":\"hamn\",\"context\":{\"cluster\":\"hamn\",\"user\":\"hamn\"}}],\"clusters\":[{\"name\":\"hamn\",\"cluster\":{\"server\":\"https://127.0.0.1:16443\"}}],\"users\":[{\"name\":\"hamn\",\"user\":{}}]}' ;;\n"
               "*'config current-context'*)\n"
               "  current=$(sed -n 's/^current-context: //p' \"$2\" | head -n 1)\n"
               "  [ -n \"$current\" ] || exit 1\n"
               "  printf '%s\\n' \"$current\" ;;\n"
               "*'config view --flatten --raw -o yaml'*)\n"
               "  printf '%s\\n' 'apiVersion: v1' 'kind: Config' ;;\n"
               "*'config use-context '*)\n"
               "  [ \"${HAMN_TEST_FAIL_ACTIVATE:-0}\" != 1 ] || exit 94\n"
               "  printf '%s\\n' 'apiVersion: v1' 'kind: Config' \"current-context: $5\" > \"$2\" ;;\n"
               "*'config unset current-context'*)\n"
               "  printf '%s\\n' 'apiVersion: v1' 'kind: Config' > \"$2\" ;;\n"
               "*) exit 95 ;;\n"
               "esac\n",
               0755);
    write_file(ssh,
               "#!/bin/sh\n"
               "set -eu\n"
               "for argument in \"$@\"; do\n"
               "  case \"$argument\" in\n"
               "  127.0.0.1:*:127.0.0.1:6443)\n"
               "    port=${argument#127.0.0.1:}; port=${port%%:*} ;;\n"
               "  esac\n"
               "done\n"
               "case \"$*\" in\n"
               "*'-O forward'*)\n"
               "  [ \"${HAMN_TEST_FAIL_FORWARD:-}\" != \"$port\" ] || exit 97\n"
               "  printf 'forward %s\\n' \"$port\" >> \"$HAMN_TEST_SSH_LOG\"\n"
               "  { grep -Fx \"$port\" \"$HAMN_TEST_FORWARD_STATE\" 2>/dev/null || true; } | grep -q . || printf '%s\\n' \"$port\" >> \"$HAMN_TEST_FORWARD_STATE\" ;;\n"
               "*'-O cancel'*)\n"
               "  printf 'cancel %s\\n' \"$port\" >> \"$HAMN_TEST_SSH_LOG\"\n"
               "  [ \"${HAMN_TEST_FAIL_CANCEL:-}\" != \"$port\" ] || exit 99\n"
               "  { grep -Fvx \"$port\" \"$HAMN_TEST_FORWARD_STATE\" 2>/dev/null || true; } > \"$HAMN_TEST_FORWARD_STATE.tmp\"\n"
               "  mv \"$HAMN_TEST_FORWARD_STATE.tmp\" \"$HAMN_TEST_FORWARD_STATE\" ;;\n"
               "*configure-k3s*lifecycle-state*)\n"
               "  set -- $HAMN_TEST_GUEST_STATE\n"
               "  printf 'active=%s enabled=%s\\n' \"$1\" \"$2\" ;;\n"
               "*configure-k3s*start*)\n"
               "  printf 'guest-start\\n' >> \"$HAMN_TEST_SSH_LOG\" ;;\n"
               "*configure-k3s*restore-state*)\n"
               "  printf '%s\\n' \"$*\" >> \"$HAMN_TEST_SSH_LOG\" ;;\n"
               "*sudo*cat*/etc/rancher/k3s/k3s.yaml*)\n"
               "  [ \"${HAMN_TEST_FAIL_SYNC:-0}\" != 1 ] || exit 98\n"
               "  printf '%s\\n' 'apiVersion: v1' 'clusters:' '- cluster:' '    server: https://127.0.0.1:6443' '  name: default' 'contexts:' '- context:' '    cluster: default' '    user: default' '  name: default' 'current-context: default' 'users:' '- name: default' '  user:' '    token: fake' ;;\n"
               "*) exit 96 ;;\n"
               "esac\n",
               0755);
    write_file(forwards, "", 0600);
    if (setenv("HOME", home, 1) != 0 || setenv("PATH", path, 1) != 0 ||
        setenv("HAMN_TEST_SSH_LOG", ssh_log, 1) != 0 ||
        setenv("HAMN_TEST_FORWARD_STATE", forwards, 1) != 0)
        fail("cannot set test environment");

    struct profile profile;
    if (profile_load(&profile, "default") != 0 || profile_save(&profile) != 0 ||
        mkdir(marker_dir, 0700) != 0)
        fail("cannot prepare profile fixture");
    char marker_text[PATH_MAX + 192];
    if (snprintf(marker_text, sizeof(marker_text),
                 "schema=1\npath=%s\ncontext=hamn\n", global) >=
        (int)sizeof(marker_text))
        fail("marker path is too long");
    write_file(marker, marker_text, 0600);

    struct vm_state state;
    memset(&state, 0, sizeof(state));
    snprintf(state.state, sizeof(state.state), "running");
    snprintf(state.ip, sizeof(state.ip), "192.0.2.20");

    write_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: foreign\n", 0600);
    write_file(local, "previous-local\n", 0600);
    char config_before[4096];
    read_file(config, config_before, sizeof(config_before));
    if (state_save(&profile, &state) != 0 ||
        setenv("HAMN_TEST_GUEST_STATE", "0 0", 1) != 0 ||
        setenv("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH", config, 1) != 0 ||
        setenv("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE", "fsync", 1) != 0)
        fail("cannot configure fresh rollback fixture");
    if (hamn_test_kubernetes_start(&profile, &state) == 0)
        fail("profile-save failure accepted a fresh Kubernetes start");
    unsetenv("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH");
    unsetenv("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE");
    expect_file(config, config_before,
                "fresh rollback did not restore the profile configuration");
    expect_file(local, "previous-local\n",
                "fresh rollback did not restore the profile kubeconfig");
    expect_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: foreign\n",
                "fresh rollback did not restore the current kube context");
    if (access(kube_port, F_OK) == 0)
        fail("fresh rollback retained a Kubernetes API port record");
    expect_file(forwards, "", "fresh rollback retained an API forward");
    char log[4096];
    read_file(ssh_log, log, sizeof(log));
    expect_contains(log, "guest-start\n", "guest K3s start was not exercised");
    expect_contains(log, "restore-state 0 0", "fresh guest state was not restored");
    struct vm_state restored;
    if (state_load(&profile, &restored) != 0 || restored.prev_kube_context[0])
        fail("fresh rollback changed previous kube context state");

    reset_fixture(ssh_log);
    write_file(forwards, "16443\n", 0600);
    write_file(kube_port, "16443\n", 0600);
    write_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: hamn\n", 0600);
    write_file(local, "cancel-failure-local\n", 0600);
    memset(&state, 0, sizeof(state));
    snprintf(state.state, sizeof(state.state), "running");
    snprintf(state.ip, sizeof(state.ip), "192.0.2.20");
    profile.kubernetes_enabled = 1;
    if (state_save(&profile, &state) != 0 || profile_save(&profile) != 0 ||
        setenv("HAMN_TEST_GUEST_STATE", "1 1", 1) != 0 ||
        setenv("HAMN_TEST_FAIL_CANCEL", "16443", 1) != 0)
        fail("cannot configure cancel rollback fixture");
    if (hamn_test_kubernetes_start(&profile, &state) == 0)
        fail("existing forward cancellation failure accepted Kubernetes start");
    unsetenv("HAMN_TEST_FAIL_CANCEL");
    expect_file(local, "cancel-failure-local\n",
                "cancel rollback changed the profile kubeconfig");
    expect_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: hamn\n",
                "cancel rollback changed the active kube context");
    expect_file(kube_port, "16443\n",
                "cancel rollback changed the Kubernetes API port");
    expect_file(forwards, "16443\n",
                "cancel rollback changed the existing API forward");
    read_file(ssh_log, log, sizeof(log));
    expect_contains(log, "cancel 16443", "existing API forward was not cancelled");
    if (strstr(log, "forward 16443"))
        fail("failed cancellation caused a duplicate API forward");
    expect_contains(log, "restore-state 1 1",
                    "cancel failure did not restore guest state");

    reset_fixture(ssh_log);
    write_file(forwards, "16443\n", 0600);
    write_file(kube_port, "16443\n", 0600);
    write_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: hamn\n", 0600);
    write_file(local, "sync-failure-local\n", 0600);
    memset(&state, 0, sizeof(state));
    snprintf(state.state, sizeof(state.state), "running");
    snprintf(state.ip, sizeof(state.ip), "192.0.2.20");
    profile.kubernetes_enabled = 1;
    if (state_save(&profile, &state) != 0 || profile_save(&profile) != 0 ||
        setenv("HAMN_TEST_GUEST_STATE", "1 1", 1) != 0 ||
        setenv("HAMN_TEST_FAIL_SYNC", "1", 1) != 0)
        fail("cannot configure sync rollback fixture");
    if (hamn_test_kubernetes_start(&profile, &state) == 0)
        fail("kubeconfig sync failure accepted Kubernetes start");
    unsetenv("HAMN_TEST_FAIL_SYNC");
    expect_file(local, "sync-failure-local\n",
                "sync rollback did not restore the profile kubeconfig");
    expect_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: hamn\n",
                "sync rollback changed the active kube context");
    expect_file(kube_port, "16443\n",
                "sync rollback did not restore the Kubernetes API port");
    expect_file(forwards, "16443\n",
                "sync rollback did not restore the API forward");
    read_file(ssh_log, log, sizeof(log));
    expect_contains(log, "cancel 16443", "sync rollback did not remove replacement forward");
    expect_contains(log, "restore-state 1 1",
                    "sync failure did not restore guest state");

    reset_fixture(ssh_log);
    write_file(forwards, "16443\n", 0600);
    write_file(kube_port, "16443\n", 0600);
    write_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: hamn\n", 0600);
    write_file(local, "activation-failure-local\n", 0600);
    memset(&state, 0, sizeof(state));
    snprintf(state.state, sizeof(state.state), "running");
    snprintf(state.ip, sizeof(state.ip), "192.0.2.20");
    profile.kubernetes_enabled = 1;
    if (state_save(&profile, &state) != 0 || profile_save(&profile) != 0 ||
        setenv("HAMN_TEST_GUEST_STATE", "1 1", 1) != 0 ||
        setenv("HAMN_TEST_FAIL_ACTIVATE", "1", 1) != 0)
        fail("cannot configure activation rollback fixture");
    if (hamn_test_kubernetes_start(&profile, &state) == 0)
        fail("kubeconfig activation failure accepted Kubernetes start");
    unsetenv("HAMN_TEST_FAIL_ACTIVATE");
    expect_file(local, "activation-failure-local\n",
                "activation rollback did not restore the profile kubeconfig");
    expect_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: hamn\n",
                "activation rollback changed the active kube context");
    expect_file(kube_port, "16443\n",
                "activation rollback did not restore the Kubernetes API port");
    expect_file(forwards, "16443\n",
                "activation rollback did not restore the API forward");
    read_file(ssh_log, log, sizeof(log));
    expect_contains(log, "cancel 16443",
                    "activation rollback did not replace the API forward");
    expect_contains(log, "restore-state 1 1",
                    "activation failure did not restore guest state");

    reset_fixture(ssh_log);
    write_file(forwards, "16443\n", 0600);
    write_file(kube_port, "16443\n", 0600);
    write_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: hamn\n", 0600);
    write_file(local, "existing-local\n", 0600);
    snprintf(state.prev_kube_context, sizeof(state.prev_kube_context), "foreign");
    profile.kubernetes_enabled = 1;
    if (state_save(&profile, &state) != 0 || profile_save(&profile) != 0 ||
        setenv("HAMN_TEST_GUEST_STATE", "1 1", 1) != 0 ||
        setenv("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH", config, 1) != 0 ||
        setenv("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE", "open", 1) != 0)
        fail("cannot configure existing rollback fixture");
    if (hamn_test_kubernetes_start(&profile, &state) == 0)
        fail("profile-save failure accepted an existing Kubernetes start");
    unsetenv("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH");
    unsetenv("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE");
    expect_file(local, "existing-local\n",
                "existing rollback did not restore the profile kubeconfig");
    expect_file(global, "apiVersion: v1\nkind: Config\ncurrent-context: hamn\n",
                "existing rollback changed the active kube context");
    expect_file(kube_port, "16443\n",
                "existing rollback did not restore the Kubernetes API port");
    expect_file(forwards, "16443\n",
                "existing rollback did not restore the API forward");
    read_file(ssh_log, log, sizeof(log));
    expect_contains(log, "restore-state 1 1",
                    "existing guest state was not restored");
    if (state_load(&profile, &restored) != 0 ||
        strcmp(restored.prev_kube_context, "foreign") != 0)
        fail("existing rollback changed previous kube context state");

    printf("PASS: Kubernetes start restores guest and host state after host failure\n");
    return 0;
}
