#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "core/guest_deployment.h"
#include "core/profile.h"

static int fail(const char *message)
{
    fprintf(stderr, "FAIL: %s\n", message);
    return 1;
}

int main(void)
{
    char home[] = "/tmp/hamn-deployment-fingerprint.XXXXXX";
    char marker[PROFILE_PATH_CAP];
    if (!mkdtemp(home) || setenv("HOME", home, 1) != 0 ||
        setenv("HAMN_SOURCE_DIR", "/nonexistent-hamn-source", 1) != 0)
        return fail("cannot prepare isolated profile state");

    struct profile profile;
    if (profile_load(&profile, "default") != 0 ||
        guest_deployment_mark_current(&profile) != 0 ||
        guest_deployment_is_current(&profile) != 1)
        return fail("initial deployment fingerprint is not current");

    snprintf(profile.docker_daemon_json, sizeof(profile.docker_daemon_json),
             "{\"debug\":true}");
    if (guest_deployment_is_current(&profile) != 0)
        return fail("Docker daemon JSON did not invalidate deployment marker");
    if (guest_deployment_mark_current(&profile) != 0 ||
        guest_deployment_is_current(&profile) != 1)
        return fail("updated Docker daemon JSON cannot mark deployment current");

    profile.rosetta = 1;
    if (guest_deployment_is_current(&profile) != 0)
        return fail("Rosetta selection did not invalidate deployment marker");
    if (guest_deployment_mark_current(&profile) != 0 ||
        guest_deployment_is_current(&profile) != 1)
        return fail("updated Rosetta selection cannot mark deployment current");

    if (!profile_path(&profile, "guest-deployment.version", marker,
                      sizeof(marker)) || unlink(marker) != 0 ||
        rmdir(profile.dir) != 0) {
        perror("cleanup profile");
        return 1;
    }
    char hamn_dir[PATH_MAX];
    int length = snprintf(hamn_dir, sizeof(hamn_dir), "%s/.hamn", home);
    if (length < 0 || length >= (int)sizeof(hamn_dir) || rmdir(hamn_dir) != 0 ||
        rmdir(home) != 0) {
        perror("cleanup home");
        return 1;
    }
    puts("PASS: immutable guest settings invalidate deployment without host sources");
    return 0;
}
