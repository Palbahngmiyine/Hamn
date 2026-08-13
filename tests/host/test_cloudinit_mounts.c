#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "core/profile.h"
#include "seed/cloudinit.h"
#include "util/proc.h"

static int write_text(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    if (fd < 0)
        return -1;
    size_t length = strlen(text);
    int written = write(fd, text, length) == (ssize_t)length;
    int closed = close(fd) == 0;
    return written && closed ? 0 : -1;
}

static int extract_user_data(const char *iso, char *output, size_t capacity)
{
    const char *command[] = { "bsdtar", "-xOf", iso, "user-data", NULL };
    return proc_run_capture(command, output, capacity) == 0 ? 0 : -1;
}

static int require_contains(const char *text, const char *expected)
{
    if (strstr(text, expected))
        return 0;
    fprintf(stderr, "missing expected seed entry: %s\n", expected);
    return -1;
}

static int require_absent(const char *text, const char *unexpected)
{
    if (!strstr(text, unexpected))
        return 0;
    fprintf(stderr, "unexpected seed entry remains: %s\n", unexpected);
    return -1;
}

static int path_join(char *output, size_t capacity, const char *base,
                     const char *name)
{
    int written = snprintf(output, capacity, "%s/%s", base, name);
    return written >= 0 && (size_t)written < capacity ? 0 : -1;
}

int main(void)
{
    char root[] = "/tmp/hamn-cloudinit-mounts.XXXXXX";
    char profile_dir[PATH_MAX], home[PATH_MAX], project[PATH_MAX];
    char external[PATH_MAX], path[PATH_MAX], iso[PATH_MAX], config[PATH_MAX];
    char user_data[32 * 1024];
    int rc = 1;
    if (!mkdtemp(root) ||
        path_join(profile_dir, sizeof(profile_dir), root, "profile") != 0 ||
        path_join(home, sizeof(home), root, "home") != 0 ||
        path_join(project, sizeof(project), home, "project") != 0 ||
        path_join(external, sizeof(external), root, "external") != 0 ||
        mkdir(profile_dir, 0700) != 0 || mkdir(home, 0700) != 0 ||
        mkdir(project, 0700) != 0 || mkdir(external, 0700) != 0 ||
        setenv("HOME", home, 1) != 0)
        goto out;

    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.dir, sizeof(profile.dir), "%s", profile_dir);
    profile.mount_home = 1;
    profile.home_read_only = 1;
    profile.rosetta = 1;
    profile.mount_count = 2;
    snprintf(profile.mounts[0].location, sizeof(profile.mounts[0].location),
             "%s", project);
    snprintf(profile.mounts[0].mount_point,
             sizeof(profile.mounts[0].mount_point), "/workspace");
    profile.mounts[0].writable = 1;
    snprintf(profile.mounts[1].location, sizeof(profile.mounts[1].location),
             "%s", external);
    snprintf(profile.mounts[1].mount_point,
             sizeof(profile.mounts[1].mount_point), "/external");

    if (path_join(path, sizeof(path), profile_dir, "id_ed25519.pub") != 0 ||
        write_text(path, "ssh-ed25519 test-key user@example.invalid\n") != 0 ||
        cloudinit_seed_ensure(&profile, 1) != 0 ||
        path_join(iso, sizeof(iso), profile_dir, "seed.iso") != 0 ||
        extract_user_data(iso, user_data, sizeof(user_data)) != 0) {
        perror("cannot create or read cloud-init seed");
        goto out;
    }
    if (require_contains(user_data,
                         "- \"ssh-ed25519 test-key user@example.invalid\"") != 0 ||
        require_contains(user_data, "groups: [sudo, hamn]") != 0 ||
        require_absent(user_data, "\"/opt/hamn\"") != 0) {
        goto out;
    }
    if (require_contains(user_data,
                         "[ \"rosetta\", \"/mnt/hamn-rosetta\", virtiofs, \"ro,nofail\", \"0\", \"0\" ]") != 0 ||
        require_absent(user_data, "package_update:") != 0 ||
        require_absent(user_data, "packages:") != 0 ||
        require_absent(user_data, "qemu-user-static") != 0 ||
        require_absent(user_data, "containerd") != 0) {
        goto out;
    }
    char expected_home[PATH_MAX + 96];
    int written = snprintf(expected_home, sizeof(expected_home),
                           "[ \"home\", \"%s\", virtiofs, \"ro,nofail\", \"0\", \"0\" ]",
                           home);
    if (written < 0 || written >= (int)sizeof(expected_home) ||
        require_contains(user_data, expected_home) != 0 ||
        require_contains(user_data,
                         "[ \"mount0\", \"/workspace\", virtiofs, \"defaults,nofail\", \"0\", \"0\" ]") != 0 ||
        require_contains(user_data,
                         "[ \"mount1\", \"/external\", virtiofs, \"ro,nofail\", \"0\", \"0\" ]") != 0) {
        goto out;
    }

    if (path_join(config, sizeof(config), profile_dir, "config.yaml") != 0 ||
        write_text(config, "# changed profile\n") != 0) {
        goto out;
    }
    struct stat seed_status;
    if (stat(iso, &seed_status) != 0)
        goto out;
    struct timespec newer[2] = {
        { .tv_sec = seed_status.st_mtimespec.tv_sec + 1, .tv_nsec = 0 },
        { .tv_sec = seed_status.st_mtimespec.tv_sec + 1, .tv_nsec = 0 },
    };
    if (utimensat(AT_FDCWD, config, newer, 0) != 0)
        goto out;
    profile.home_read_only = 0;
    if (cloudinit_seed_ensure(&profile, 0) != 0 ||
        extract_user_data(iso, user_data, sizeof(user_data)) != 0) {
        perror("cannot refresh or read cloud-init seed");
        goto out;
    }
    written = snprintf(expected_home, sizeof(expected_home),
                       "[ \"home\", \"%s\", virtiofs, \"defaults,nofail\", \"0\", \"0\" ]",
                       home);
    if (written < 0 || written >= (int)sizeof(expected_home) ||
        require_contains(user_data, expected_home) != 0)
        goto out;

    rc = 0;
out:
    (void)unlink(iso);
    if (path_join(path, sizeof(path), profile_dir, "id_ed25519.pub") == 0)
        (void)unlink(path);
    (void)unlink(config);
    (void)rmdir(project);
    (void)rmdir(home);
    (void)rmdir(external);
    (void)rmdir(profile_dir);
    (void)rmdir(root);
    if (rc == 0)
        puts("PASS: cloud-init seed mounts and config refresh");
    return rc;
}
