#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "cli.h"
#include "core/log.h"
#include "util/proc.h"

static void update_usage(FILE *stream)
{
    fprintf(stream, "usage: hamn update [--manifest URL_OR_PATH]\n");
}

static int path_parent(const char *path, char output[PATH_MAX])
{
    const char *slash = path ? strrchr(path, '/') : NULL;
    if (!slash || slash == path)
        return -1;
    size_t length = (size_t)(slash - path);
    if (length >= PATH_MAX)
        return -1;
    memcpy(output, path, length);
    output[length] = '\0';
    return 0;
}

static int resolve_invocation(char output[PATH_MAX])
{
    const char *input = cli_invocation_path();
    if (!input || !input[0])
        return -1;
    if (strchr(input, '/')) {
        if (input[0] == '/') {
            if (snprintf(output, PATH_MAX, "%s", input) >= PATH_MAX)
                return -1;
        } else {
            char current[PATH_MAX];
            if (!getcwd(current, sizeof(current)) ||
                snprintf(output, PATH_MAX, "%s/%s", current, input) >=
                    PATH_MAX)
                return -1;
        }
    } else {
        const char *path = getenv("PATH");
        if (!path)
            return -1;
        const char *cursor = path;
        for (;;) {
            const char *separator = strchr(cursor, ':');
            size_t length = separator ? (size_t)(separator - cursor) :
                strlen(cursor);
            char directory[PATH_MAX];
            if (length == 0) {
                if (!getcwd(directory, sizeof(directory)))
                    return -1;
            } else if (length < sizeof(directory)) {
                memcpy(directory, cursor, length);
                directory[length] = '\0';
            } else {
                return -1;
            }
            if (snprintf(output, PATH_MAX, "%s/%s", directory, input) <
                    PATH_MAX &&
                access(output, X_OK) == 0)
                break;
            if (!separator)
                return -1;
            cursor = separator + 1;
        }
    }
    struct stat status;
    return lstat(output, &status) == 0 ? 0 : -1;
}

static int managed_paths(char executable[PATH_MAX], char datadir[PATH_MAX],
                         char helper[PATH_MAX])
{
    if (!proc_self_path(executable, PATH_MAX))
        return -1;
    const char marker[] = "/.hamn-generations/";
    char *generation = strstr(executable, marker);
    if (!generation)
        return -1;
    char *suffix = strchr(generation + sizeof(marker) - 1, '/');
    if (!suffix || strcmp(suffix, "/bin/hamn") != 0)
        return -1;
    size_t data_length = (size_t)(generation - executable);
    if (data_length == 0 || data_length >= PATH_MAX)
        return -1;
    memcpy(datadir, executable, data_length);
    datadir[data_length] = '\0';
    size_t generation_length = (size_t)(suffix - executable);
    int written = snprintf(helper, PATH_MAX,
                           "%.*s/share/hamn/src/scripts/update-host.sh",
                           (int)generation_length, executable);
    if (written < 0 || written >= PATH_MAX)
        return -1;
    struct stat status;
    return lstat(helper, &status) == 0 && S_ISREG(status.st_mode) &&
        status.st_uid == geteuid() && status.st_nlink == 1 &&
        (status.st_mode & 0111) != 0 ? 0 : -1;
}

int cmd_update(int argc, char **argv)
{
    const char *manifest = NULL;
    for (int index = 1; index < argc; index++) {
        if (strcmp(argv[index], "--manifest") == 0) {
            if (++index >= argc || manifest || !argv[index][0]) {
                update_usage(stderr);
                return 2;
            }
            manifest = argv[index];
        } else if (strcmp(argv[index], "--help") == 0 ||
                   strcmp(argv[index], "-h") == 0) {
            update_usage(stdout);
            return 0;
        } else {
            update_usage(stderr);
            return 2;
        }
    }

    char executable[PATH_MAX], datadir[PATH_MAX], helper[PATH_MAX];
    if (managed_paths(executable, datadir, helper) != 0) {
        logerr("update requires a managed Hamn installation; reinstall with the signed installer");
        return 1;
    }
    char invocation[PATH_MAX], resolved[PATH_MAX], binary_dir[PATH_MAX];
    struct stat invocation_status;
    if (resolve_invocation(invocation) != 0 ||
        lstat(invocation, &invocation_status) != 0 ||
        !S_ISLNK(invocation_status.st_mode) || !realpath(invocation, resolved) ||
        strcmp(resolved, executable) != 0 ||
        path_parent(invocation, binary_dir) != 0) {
        logerr("update requires the managed hamn command symlink, not a direct generation binary");
        return 1;
    }

    const char *command[10] = {
        "bash", helper, "--bindir", binary_dir, "--datadir", datadir,
        NULL, NULL, NULL,
    };
    size_t count = 6;
    if (manifest) {
        command[count++] = "--manifest";
        command[count++] = manifest;
    }
    command[count] = NULL;
    int rc = proc_run(command);
    if (rc != 0) {
        logerr("update failed; the previous binary and guest image selection remain active");
        return 1;
    }
    logmsg("update installed atomically; the selected guest image is used for new profile disks; existing profile disks keep their current guest root");
    return 0;
}
