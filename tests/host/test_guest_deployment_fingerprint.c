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
        !profile_path(&profile, "guest-deployment.version", marker,
                      sizeof(marker)))
        return fail("cannot load the default profile");

    /* The marker that an earlier Hamn wrote for these default settings, whose
     * fingerprint also hashed "rosetta" and "false". It describes a deployment
     * that ran configure-rosetta, so the next start refreshes it. */
    FILE *released = fopen(marker, "w");
    if (!released ||
        fputs("schemaVersion=6\nfingerprint=sha256:"
              "73d25d2686d730b37d08f4563db55f695ffac4f5e486094dc5164a97fb2073cb\n",
              released) < 0 ||
        fclose(released) != 0)
        return fail("cannot write the marker of an earlier Hamn");
    if (guest_deployment_is_current(&profile) != 0)
        return fail("the marker of an earlier Hamn's deployment is current");

    if (guest_deployment_mark_current(&profile) != 0 ||
        guest_deployment_is_current(&profile) != 1)
        return fail("initial deployment fingerprint is not current");

    snprintf(profile.docker_daemon_json, sizeof(profile.docker_daemon_json),
             "{\"debug\":true}");
    if (guest_deployment_is_current(&profile) != 0)
        return fail("Docker daemon JSON did not invalidate deployment marker");
    if (guest_deployment_mark_current(&profile) != 0 ||
        guest_deployment_is_current(&profile) != 1)
        return fail("updated Docker daemon JSON cannot mark deployment current");

    if (unlink(marker) != 0 || rmdir(profile.dir) != 0) {
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
