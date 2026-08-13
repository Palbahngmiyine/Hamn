#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "core/kubeconfig.h"
#include "core/profile.h"
#include "core/state.h"

static void fail(const char *message)
{
    fprintf(stderr, "FAIL: %s\n", message);
    exit(1);
}

static void write_file(const char *path, const char *text, mode_t mode)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, mode);
    if (fd < 0)
        fail("cannot create test fixture");
    size_t length = strlen(text);
    if (write(fd, text, length) != (ssize_t)length || fchmod(fd, mode) != 0 ||
        close(fd) != 0)
        fail("cannot write test fixture");
}

static void read_file(const char *path, char *output, size_t cap)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        fail("cannot read test fixture");
    ssize_t count = read(fd, output, cap - 1);
    if (count < 0 || close(fd) != 0)
        fail("cannot read test fixture");
    output[count] = '\0';
}

static void expect_contains(const char *text, const char *needle,
                            const char *message)
{
    if (!strstr(text, needle))
        fail(message);
}

int main(void)
{
    char work[] = "/tmp/hamn-kubecontext.XXXXXX";
    if (!mkdtemp(work))
        fail("cannot create test workspace");
    char home[PATH_MAX], hamn_dir[PATH_MAX], profile_dir[PATH_MAX], kube_dir[PATH_MAX];
    char local[PATH_MAX], global[PATH_MAX], marker[PATH_MAX], bin[PATH_MAX];
    char kubectl[PATH_MAX], log[PATH_MAX], path[PATH_MAX * 2];
    if (snprintf(home, sizeof(home), "%s/home", work) >= (int)sizeof(home) ||
        snprintf(hamn_dir, sizeof(hamn_dir), "%s/.hamn", home) >=
            (int)sizeof(hamn_dir) ||
        snprintf(profile_dir, sizeof(profile_dir), "%s/.hamn/default", home) >=
            (int)sizeof(profile_dir) ||
        snprintf(kube_dir, sizeof(kube_dir), "%s/.kube", home) >=
            (int)sizeof(kube_dir) ||
        snprintf(local, sizeof(local), "%s/kubeconfig", profile_dir) >=
            (int)sizeof(local) ||
        snprintf(global, sizeof(global), "%s/config", kube_dir) >=
            (int)sizeof(global) ||
        snprintf(marker, sizeof(marker), "%s/.hamn/.kube-contexts/default",
                 home) >= (int)sizeof(marker) ||
        snprintf(bin, sizeof(bin), "%s/bin", work) >= (int)sizeof(bin) ||
        snprintf(kubectl, sizeof(kubectl), "%s/kubectl", bin) >=
            (int)sizeof(kubectl) ||
        snprintf(log, sizeof(log), "%s/kubectl.log", work) >= (int)sizeof(log) ||
        snprintf(path, sizeof(path), "%s:/usr/bin:/bin", bin) >= (int)sizeof(path))
        fail("test path is too long");
    if (mkdir(home, 0700) != 0 || mkdir(hamn_dir, 0700) != 0 ||
        mkdir(profile_dir, 0700) != 0 ||
        mkdir(kube_dir, 0700) != 0 || mkdir(bin, 0700) != 0)
        fail("cannot create test directories");

    write_file(local, "apiVersion: v1\nkind: Config\n", 0600);
    write_file(global, "apiVersion: v1\nkind: Config\n", 0600);
    write_file(kubectl,
               "#!/bin/sh\n"
               "set -eu\n"
               "printf '%s\\n' \"$*\" >> \"$HAMN_TEST_KUBECTL_LOG\"\n"
               "case \"$*\" in\n"
               "*'config view --raw -o json')\n"
               "  case \"${HAMN_TEST_KUBECTL_MODE:-empty}\" in\n"
               "  empty) printf '%s\\n' '{\"apiVersion\":\"v1\",\"kind\":\"Config\"}' ;;\n"
               "  owned) printf '{\"contexts\":[{\"name\":\"%s\",\"context\":{\"cluster\":\"%s\",\"user\":\"%s\"}}],\"clusters\":[{\"name\":\"%s\",\"cluster\":{\"server\":\"https://127.0.0.1:16443\"}}],\"users\":[{\"name\":\"%s\",\"user\":{}}]}\\n' \"$HAMN_TEST_KUBECTL_CONTEXT\" \"$HAMN_TEST_KUBECTL_CONTEXT\" \"$HAMN_TEST_KUBECTL_CONTEXT\" \"$HAMN_TEST_KUBECTL_CONTEXT\" \"$HAMN_TEST_KUBECTL_CONTEXT\" ;;\n"
               "  foreign) printf '{\"contexts\":[{\"name\":\"%s\",\"context\":{\"cluster\":\"foreign\",\"user\":\"foreign\"}}],\"clusters\":[{\"name\":\"%s\",\"cluster\":{\"server\":\"https://127.0.0.1:16443\"}}],\"users\":[{\"name\":\"%s\",\"user\":{}}]}\\n' \"$HAMN_TEST_KUBECTL_CONTEXT\" \"$HAMN_TEST_KUBECTL_CONTEXT\" \"$HAMN_TEST_KUBECTL_CONTEXT\" ;;\n"
               "  esac ;;\n"
               "*'config current-context')\n"
               "  [ \"${HAMN_TEST_KUBECTL_CURRENT:-none}\" != none ] || exit 1\n"
               "  printf '%s\\n' \"$HAMN_TEST_KUBECTL_CURRENT\" ;;\n"
               "*'config view --flatten --raw -o yaml')\n"
               "  printf '%s\\n' 'apiVersion: v1' 'kind: Config' \"current-context: $HAMN_TEST_KUBECTL_CONTEXT\" ;;\n"
               "*'config use-context '*)\n"
               "  printf '%s\\n' 'apiVersion: v1' 'kind: Config' \"current-context: $5\" > \"$2\" ;;\n"
               "*) exit 95 ;;\n"
               "esac\n",
               0755);
    if (setenv("HOME", home, 1) != 0 || setenv("PATH", path, 1) != 0 ||
        setenv("HAMN_TEST_KUBECTL_LOG", log, 1) != 0 ||
        setenv("HAMN_TEST_KUBECTL_MODE", "empty", 1) != 0 ||
        setenv("HAMN_TEST_KUBECTL_CONTEXT", "hamn", 1) != 0 ||
        setenv("HAMN_TEST_KUBECTL_CURRENT", "foreign", 1) != 0)
        fail("cannot set test environment");

    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.name, sizeof(profile.name), "default");
    snprintf(profile.dir, sizeof(profile.dir), "%s", profile_dir);
    struct vm_state state;
    memset(&state, 0, sizeof(state));
    snprintf(state.state, sizeof(state.state), "running");

    char symlink_target[PATH_MAX];
    if (snprintf(symlink_target, sizeof(symlink_target), "%s/foreign-config",
                 work) >= (int)sizeof(symlink_target))
        fail("symlink fixture path is too long");
    write_file(symlink_target, "apiVersion: v1\nkind: Config\n", 0600);
    if (unlink(global) != 0 || symlink(symlink_target, global) != 0)
        fail("cannot create unsafe kubeconfig fixture");
    if (kubeconfig_preflight_profile(&profile) == 0)
        fail("symlinked global kubeconfig was accepted");
    if (unlink(global) != 0)
        fail("cannot remove unsafe kubeconfig fixture");
    write_file(global, "apiVersion: v1\nkind: Config\n", 0600);

    if (kubeconfig_activate_profile(&profile, &state) != 0)
        fail("first profile context activation failed");
    if (strcmp(state.prev_kube_context, "foreign") != 0)
        fail("previous kube context was not saved");
    char text[4096];
    read_file(global, text, sizeof(text));
    expect_contains(text, "current-context: hamn",
                    "merged kubeconfig was not atomically published");
    read_file(marker, text, sizeof(text));
    expect_contains(text, "context=hamn\n", "ownership marker is missing");

    if (setenv("HAMN_TEST_KUBECTL_MODE", "owned", 1) != 0 ||
        setenv("HAMN_TEST_KUBECTL_CURRENT", "hamn", 1) != 0)
        fail("cannot configure owned context fixture");
    if (kubeconfig_activate_profile(&profile, &state) != 0)
        fail("owned Hamn context was not refreshable");
    if (strcmp(state.prev_kube_context, "foreign") != 0)
        fail("refresh replaced the original previous context");

    read_file(global, text, sizeof(text));
    if (setenv("HAMN_TEST_KUBECTL_MODE", "foreign", 1) != 0)
        fail("cannot configure foreign context fixture");
    if (kubeconfig_activate_profile(&profile, &state) == 0)
        fail("foreign kube context collision was overwritten");
    char after[4096];
    read_file(global, after, sizeof(after));
    if (strcmp(text, after) != 0)
        fail("foreign kube context collision changed global config");

    if (setenv("HAMN_TEST_KUBECTL_MODE", "owned", 1) != 0 ||
        setenv("HAMN_TEST_KUBECTL_CURRENT", "hamn", 1) != 0)
        fail("cannot configure restore fixture");
    if (kubeconfig_restore_previous(&profile, &state) != 0)
        fail("previous kube context restoration failed");
    if (state.prev_kube_context[0])
        fail("restored context was not cleared from state");
    read_file(log, text, sizeof(text));
    expect_contains(text, "config use-context foreign",
                    "previous kube context was not activated");
    read_file(global, text, sizeof(text));
    expect_contains(text, "current-context: foreign",
                    "previous kube context was not atomically published");

    snprintf(profile.name, sizeof(profile.name), "work");
    if (setenv("HAMN_TEST_KUBECTL_CONTEXT", "hamn-work", 1) != 0 ||
        setenv("HAMN_TEST_KUBECTL_MODE", "foreign", 1) != 0)
        fail("cannot configure named context collision fixture");
    if (kubeconfig_preflight_profile(&profile) == 0)
        fail("foreign named kube context collision was accepted");

    unlink(log);
    unlink(kubectl);
    unlink(marker);
    unlink(symlink_target);
    unlink(global);
    unlink(local);
    char state_path[PATH_MAX];
    snprintf(state_path, sizeof(state_path), "%s/state.json", profile_dir);
    unlink(state_path);
    rmdir(bin);
    rmdir(kube_dir);
    rmdir(profile_dir);
    char context_dir[PATH_MAX];
    snprintf(context_dir, sizeof(context_dir), "%s/.kube-contexts", hamn_dir);
    rmdir(context_dir);
    rmdir(hamn_dir);
    rmdir(home);
    rmdir(work);
    printf("PASS: K3s kube context ownership and restoration are isolated\n");
    return 0;
}
