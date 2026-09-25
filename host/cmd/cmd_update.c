#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "cli.h"
#include "cjson/cJSON.h"
#include "core/control.h"
#include "core/log.h"
#include "util/proc.h"

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

static int unsupported_check_result(char **result)
{
    cJSON *value = cJSON_CreateObject();
    cJSON *artifacts = NULL;
    if (!value || !cJSON_AddNumberToObject(value, "schemaVersion", 1) ||
        !cJSON_AddStringToObject(value, "currentVersion", HAMN_VERSION) ||
        !cJSON_AddNullToObject(value, "latestVersion") ||
        !cJSON_AddStringToObject(value, "status", "unsupported-install") ||
        !cJSON_AddNumberToObject(value, "downloadedBytes", 0) ||
        !cJSON_AddNumberToObject(value, "resumedBytes", 0) ||
        !cJSON_AddNumberToObject(value, "reusedBytes", 0) ||
        !cJSON_AddBoolToObject(value, "completed", 1) ||
        !cJSON_AddBoolToObject(value, "profileDisksChanged", 0) ||
        !(artifacts = cJSON_AddObjectToObject(value, "artifacts"))) {
        cJSON_Delete(value);
        return 1;
    }
    const char *names[] = { "manifest", "host", "guestImage" };
    for (size_t index = 0; index < sizeof(names) / sizeof(names[0]); index++) {
        cJSON *artifact = cJSON_AddObjectToObject(artifacts, names[index]);
        if (!artifact || !cJSON_AddNumberToObject(artifact, "downloadedBytes", 0) ||
            !cJSON_AddNumberToObject(artifact, "resumedBytes", 0) ||
            !cJSON_AddNumberToObject(artifact, "reusedBytes", 0) ||
            !cJSON_AddStringToObject(artifact, "source", "none")) {
            cJSON_Delete(value);
            return 1;
        }
    }
    *result = cJSON_PrintUnformatted(value);
    cJSON_Delete(value);
    return *result ? 0 : 1;
}

/* The updater writes one human failure reason (a single line) into the
 * private result file when it fails. Accept only bounded printable text. */
static int failure_reason(int fd, char *reason, size_t capacity)
{
    struct stat status;
    if (fstat(fd, &status) != 0 || status.st_size <= 0 ||
        (size_t)status.st_size >= capacity)
        return -1;
    ssize_t length = pread(fd, reason, (size_t)status.st_size, 0);
    if (length != status.st_size)
        return -1;
    while (length > 0 && reason[length - 1] == '\n')
        length--;
    if (length == 0)
        return -1;
    for (ssize_t index = 0; index < length; index++) {
        unsigned char byte = (unsigned char)reason[index];
        if (byte < 0x20 || byte == 0x7f)
            return -1;
    }
    reason[length] = '\0';
    return 0;
}

int hamn_control_upgrade(const char *manifest, int check_only, int force,
                         char **result)
{
    if (!result || (check_only != 0 && check_only != 1) ||
        (force != 0 && force != 1) || (check_only && force))
        return 2;
    *result = NULL;
    char executable[PATH_MAX], datadir[PATH_MAX], helper[PATH_MAX];
    if (managed_paths(executable, datadir, helper) != 0) {
        if (check_only)
            return unsupported_check_result(result);
        log_set_error("this hamn is not a managed installation, so it cannot upgrade itself; "
                      "install Hamn with the official installer: "
                      "https://github.com/Palbahngmiyine/Hamn#install");
        return 1;
    }
    char invocation[PATH_MAX], resolved[PATH_MAX], binary_dir[PATH_MAX];
    struct stat invocation_status;
    if (resolve_invocation(invocation) != 0 ||
        lstat(invocation, &invocation_status) != 0 ||
        !S_ISLNK(invocation_status.st_mode) || !realpath(invocation, resolved) ||
        strcmp(resolved, executable) != 0 ||
        path_parent(invocation, binary_dir) != 0) {
        if (check_only)
            return unsupported_check_result(result);
        log_set_error("update requires the managed hamn command symlink, not a direct generation binary; run hamn upgrade");
        return 1;
    }

    char result_directory[] = "/tmp/hamn-upgrade-result.XXXXXX";
    if (!mkdtemp(result_directory)) {
        logerr("cannot create private upgrade result directory");
        return 1;
    }
    char result_path[PATH_MAX];
    snprintf(result_path, sizeof(result_path), "%s/result.json", result_directory);
    int result_fd = open(result_path, O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (result_fd < 0) {
        rmdir(result_directory);
        return 1;
    }
    struct stat result_identity;
    if (fstat(result_fd, &result_identity) != 0) {
        close(result_fd); unlink(result_path); rmdir(result_directory);
        return 1;
    }
    /* The documented system shell, independent of the caller's PATH. */
    const char *command[20] = {
        "/bin/bash", helper, "--bindir", binary_dir, "--datadir", datadir,
        NULL, NULL, NULL,
    };
    size_t count = 6;
    command[count++] = "--current-version";
    command[count++] = HAMN_VERSION;
    command[count++] = "--output-json";
    command[count++] = "--result-file";
    command[count++] = result_path;
    if (check_only)
        command[count++] = "--check-only";
    if (force)
        command[count++] = "--force";
    if (manifest) {
        command[count++] = "--manifest";
        command[count++] = manifest;
    }
    command[count] = NULL;
    /* proc_run_capture_checked merges stderr with stdout. Keep human progress
     * live and the machine result in an owned bounded private file instead. */
    int rc = proc_run(command);
    char output[16384];
    struct stat final_identity;
    ssize_t length = -1;
    if (rc == 0 && fstat(result_fd, &final_identity) == 0 &&
        final_identity.st_dev == result_identity.st_dev &&
        final_identity.st_ino == result_identity.st_ino &&
        final_identity.st_nlink == 1 && final_identity.st_size > 0 &&
        final_identity.st_size < (off_t)sizeof(output)) {
        length = pread(result_fd, output, (size_t)final_identity.st_size, 0);
        if (length != final_identity.st_size)
            length = -1;
    }
    char reason[1024];
    int reported = rc != 0 && failure_reason(result_fd, reason, sizeof(reason)) == 0;
    close(result_fd);
    unlink(result_path);
    rmdir(result_directory);
    if (reported) {
        log_set_error("%s", reason);
        return 1;
    }
    if (rc != 0 || length < 0) {
        /* proc_run reports -1 when the updater could not start or was
         * terminated by a signal; its own reason is then unavailable. */
        if (rc < 0)
            log_set_error("the updater could not run to completion; run the same command again");
        else if (check_only)
            log_set_error("could not check for updates; run the same command again");
        else
            log_set_error("upgrade did not complete; run the same command again to retry "
                          "(an interrupted upgrade is recovered automatically%s)",
                          manifest ? ", keep the same --manifest" : "");
        return 1;
    }
    output[length] = '\0';
    *result = strdup(output);
    if (!*result) {
        logerr("cannot allocate upgrade result");
        return 1;
    }
    return 0;
}
