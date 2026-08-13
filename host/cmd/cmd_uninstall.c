#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <fts.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "cli.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"
#include "util/proc.h"

struct uninstall_plan {
    char runtime_root[PATH_MAX];
    int runtime_exists;
    char install_root[PATH_MAX];
    int install_exists;
    char executable[PATH_MAX];
    char invocation_link[PATH_MAX];
    int invocation_link_exists;
    char binary_lock[PATH_MAX];
    int binary_lock_exists;
    char data_lock[PATH_MAX];
    int data_lock_exists;
};

static int path_join(char *output, size_t capacity, const char *parent,
                     const char *child)
{
    int written = snprintf(output, capacity, "%s/%s", parent, child);
    return written >= 0 && (size_t)written < capacity ? 0 : -1;
}

static int path_parent(const char *path, char *output, size_t capacity)
{
    if (!path || path[0] != '/')
        return -1;
    const char *slash = strrchr(path, '/');
    if (!slash || !slash[1])
        return -1;
    size_t length = slash == path ? 1 : (size_t)(slash - path);
    if (length + 1 > capacity)
        return -1;
    memcpy(output, path, length);
    output[length] = '\0';
    return 0;
}

static int path_is_within(const char *base, const char *path)
{
    size_t length = strlen(base);
    return length > 0 && strncmp(base, path, length) == 0 &&
        (base[length - 1] == '/' || path[length] == '\0' ||
         path[length] == '/');
}

static int safe_owned_directory(const char *path)
{
    struct stat status;
    return lstat(path, &status) == 0 && S_ISDIR(status.st_mode) &&
        status.st_uid == geteuid() && (status.st_mode & 022) == 0;
}

static int safe_owned_regular(const char *path, mode_t mode)
{
    struct stat status;
    return lstat(path, &status) == 0 && S_ISREG(status.st_mode) &&
        status.st_uid == geteuid() && status.st_nlink == 1 &&
        (status.st_mode & 0777) == mode;
}

static int read_small_regular(const char *path, char *output, size_t capacity)
{
    if (capacity < 2)
        return -1;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return -1;
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_uid != geteuid() || status.st_nlink != 1 ||
        status.st_size < 0 || (unsigned long long)status.st_size >= capacity) {
        (void)close(fd);
        return -1;
    }
    size_t length = 0;
    while (length < (size_t)status.st_size) {
        ssize_t count = read(fd, output + length,
                             (size_t)status.st_size - length);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            (void)close(fd);
            return -1;
        }
        if (count == 0) {
            (void)close(fd);
            return -1;
        }
        length += (size_t)count;
    }
    if (close(fd) != 0)
        return -1;
    output[length] = '\0';
    return 0;
}

static int data_marker_valid(const char *install_root)
{
    char marker[PATH_MAX], content[32];
    if (path_join(marker, sizeof(marker), install_root, ".hamn-managed") != 0 ||
        (!safe_owned_regular(marker, 0600) &&
         !safe_owned_regular(marker, 0644)) ||
        read_small_regular(marker, content, sizeof(content)) != 0)
        return 0;
    return strcmp(content, "version=1\n") == 0 || content[0] == '\0';
}

static int generation_name_valid(const char *name)
{
    if (!name || strlen(name) != 71 || name[64] != '-')
        return 0;
    for (size_t index = 0; index < 64; index++) {
        char character = name[index];
        if (!((character >= '0' && character <= '9') ||
              (character >= 'a' && character <= 'f')))
            return 0;
    }
    for (size_t index = 65; index < 71; index++) {
        char character = name[index];
        if (!((character >= '0' && character <= '9') ||
              (character >= 'a' && character <= 'z') ||
              (character >= 'A' && character <= 'Z')))
            return 0;
    }
    return 1;
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
                path_join(output, PATH_MAX, current, input) != 0)
                return -1;
        }
    } else {
        const char *path = getenv("PATH");
        if (!path)
            return -1;
        const char *cursor = path;
        while (1) {
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
            if (path_join(output, PATH_MAX, directory, input) == 0 &&
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

static int discover_installation(struct uninstall_plan *plan)
{
    char executable[PATH_MAX];
    if (!proc_self_path(executable, sizeof(executable)))
        return 0;
    const char marker[] = "/.hamn-generations/";
    char *generation = strstr(executable, marker);
    if (!generation)
        return 0;
    char *name = generation + sizeof(marker) - 1;
    char *suffix = strchr(name, '/');
    char generation_name[72];
    size_t name_length = suffix ? (size_t)(suffix - name) : 0;
    if (!suffix || name_length >= sizeof(generation_name) ||
        strcmp(suffix, "/bin/hamn") != 0) {
        logerr("refusing untrusted Hamn installation path: %s", executable);
        return -1;
    }
    memcpy(generation_name, name, name_length);
    generation_name[name_length] = '\0';
    if (!generation_name_valid(generation_name)) {
        logerr("refusing untrusted Hamn installation path: %s", executable);
        return -1;
    }
    size_t root_length = (size_t)(generation - executable);
    if (root_length == 0 || root_length >= sizeof(plan->install_root)) {
        logerr("cannot derive the managed installation root");
        return -1;
    }
    memcpy(plan->install_root, executable, root_length);
    plan->install_root[root_length] = '\0';
    if (!safe_owned_directory(plan->install_root) ||
        !data_marker_valid(plan->install_root)) {
        logerr("refusing unmanaged or unsafe installation root: %s",
               plan->install_root);
        return -1;
    }
    char generations[PATH_MAX], generation_path[PATH_MAX];
    if (path_join(generations, sizeof(generations), plan->install_root,
                  ".hamn-generations") != 0 ||
        path_join(generation_path, sizeof(generation_path), generations,
                  generation_name) != 0 || !safe_owned_directory(generations) ||
        !safe_owned_directory(generation_path) ||
        !safe_owned_regular(executable, 0755)) {
        logerr("refusing unsafe managed installation contents");
        return -1;
    }
    plan->install_exists = 1;
    snprintf(plan->executable, sizeof(plan->executable), "%s", executable);

    char invocation[PATH_MAX], resolved[PATH_MAX];
    struct stat invocation_status;
    if (resolve_invocation(invocation) == 0 &&
        lstat(invocation, &invocation_status) == 0 &&
        S_ISLNK(invocation_status.st_mode) && realpath(invocation, resolved) &&
        strcmp(resolved, executable) == 0) {
        snprintf(plan->invocation_link, sizeof(plan->invocation_link), "%s",
                 invocation);
        plan->invocation_link_exists = 1;
        char bin_parent[PATH_MAX];
        if (path_parent(invocation, bin_parent, sizeof(bin_parent)) == 0 &&
            path_join(plan->binary_lock, sizeof(plan->binary_lock), bin_parent,
                      ".hamn-install.lock") == 0 &&
            safe_owned_regular(plan->binary_lock, 0600))
            plan->binary_lock_exists = 1;
    }
    char data_parent[PATH_MAX], data_name[PATH_MAX];
    if (path_parent(plan->install_root, data_parent, sizeof(data_parent)) == 0) {
        const char *name_start = strrchr(plan->install_root, '/');
        if (name_start && name_start[1] &&
            snprintf(data_name, sizeof(data_name), ".%s.hamn-install.lock",
                     name_start + 1) < (int)sizeof(data_name) &&
            path_join(plan->data_lock, sizeof(plan->data_lock), data_parent,
                      data_name) == 0 &&
            safe_owned_regular(plan->data_lock, 0600))
            plan->data_lock_exists = 1;
    }
    return 0;
}

static int discover_runtime(struct uninstall_plan *plan)
{
    if (!hamn_home(plan->runtime_root, sizeof(plan->runtime_root))) {
        logerr("HOME is not set");
        return -1;
    }
    struct stat status;
    if (lstat(plan->runtime_root, &status) != 0) {
        if (errno == ENOENT)
            return 0;
        logerr("cannot inspect Hamn runtime data: %s", strerror(errno));
        return -1;
    }
    if (!S_ISDIR(status.st_mode) || status.st_uid != geteuid() ||
        (status.st_mode & 022) != 0) {
        logerr("refusing unsafe Hamn runtime path: %s", plan->runtime_root);
        return -1;
    }
    plan->runtime_exists = 1;
    return 0;
}

static int tree_size_bytes(const char *path, unsigned long long *total)
{
    char *roots[] = { (char *)path, NULL };
    FTS *tree = fts_open(roots, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
    if (!tree)
        return -1;
    unsigned long long sum = 0;
    FTSENT *entry;
    while ((entry = fts_read(tree))) {
        if (entry->fts_info == FTS_ERR || entry->fts_info == FTS_DNR ||
            entry->fts_info == FTS_NS || entry->fts_info == FTS_DC) {
            (void)fts_close(tree);
            return -1;
        }
        if (entry->fts_info == FTS_F) {
            if (entry->fts_statp->st_size < 0 ||
                ULLONG_MAX - sum < (unsigned long long)entry->fts_statp->st_size) {
                (void)fts_close(tree);
                return -1;
            }
            sum += (unsigned long long)entry->fts_statp->st_size;
        }
    }
    if (fts_close(tree) != 0)
        return -1;
    *total = sum;
    return 0;
}

static void print_size(const char *path)
{
    unsigned long long bytes;
    if (tree_size_bytes(path, &bytes) == 0)
        printf(" (%llu bytes)", bytes);
    else
        printf(" (size unavailable)");
}

static void print_runtime_contents(const char *root)
{
    DIR *directory = opendir(root);
    if (!directory)
        return;
    struct dirent *entry;
    while ((entry = readdir(directory))) {
        if (strcmp(entry->d_name, ".") == 0 ||
            strcmp(entry->d_name, "..") == 0)
            continue;
        char path[PATH_MAX];
        if (path_join(path, sizeof(path), root, entry->d_name) != 0)
            continue;
        if (strcmp(entry->d_name, "cache") == 0)
            printf("    image cache: %s", path);
        else if (profile_name_valid(entry->d_name))
            printf("    profile %s (VM, Docker, and Kubernetes data): %s",
                   entry->d_name, path);
        else
            printf("    Hamn state: %s", path);
        print_size(path);
        fputc('\n', stdout);
    }
    (void)closedir(directory);
}

static void print_plan(const struct uninstall_plan *plan)
{
    puts("Hamn uninstall will remove the following managed paths:");
    if (plan->install_exists) {
        printf("  managed installation root: %s", plan->install_root);
        print_size(plan->install_root);
        fputc('\n', stdout);
        if (plan->invocation_link_exists)
            printf("  managed executable link: %s\n", plan->invocation_link);
        if (plan->binary_lock_exists)
            printf("  installation lock: %s\n", plan->binary_lock);
        if (plan->data_lock_exists)
            printf("  installation lock: %s\n", plan->data_lock);
    } else {
        puts("  managed installation files: not detected for this executable");
    }
    if (plan->runtime_exists) {
        printf("  Hamn runtime root: %s", plan->runtime_root);
        print_size(plan->runtime_root);
        fputc('\n', stdout);
        print_runtime_contents(plan->runtime_root);
    } else {
        printf("  Hamn runtime root: %s (absent)\n", plan->runtime_root);
    }
}

static int confirm_uninstall(void)
{
    fputs("Type exactly y to permanently remove these paths: ", stderr);
    fflush(stderr);
    char answer[8];
    if (!fgets(answer, sizeof(answer), stdin) || strcmp(answer, "y\n") != 0) {
        logmsg("aborted");
        return 0;
    }
    return 1;
}

static int stop_profile_for_uninstall(const char *root, const char *name)
{
    char path[PATH_MAX];
    struct stat status;
    if (path_join(path, sizeof(path), root, name) != 0 ||
        lstat(path, &status) != 0 || !safe_owned_directory(path))
        return 0;
    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.name, sizeof(profile.name), "%s", name);
    snprintf(profile.dir, sizeof(profile.dir), "%s", path);
    struct vm_lifecycle_lock lifecycle;
    if (vm_lifecycle_lock_acquire(name, &lifecycle) != 0) {
        logerr("cannot lock profile %s for uninstall", name);
        return -1;
    }
    int mutation = profile_mutation_lock(&profile);
    if (mutation < 0) {
        vm_lifecycle_lock_release(&lifecycle);
        logerr("cannot lock profile %s for uninstall", name);
        return -1;
    }
    int rc = vm_stop(&profile, NULL) == VM_STOP_OK ? 0 : -1;
    profile_mutation_unlock(mutation);
    vm_lifecycle_lock_release(&lifecycle);
    if (rc != 0)
        logerr("cannot safely stop profile %s; nothing was removed", name);
    return rc;
}

static int stop_all_profiles(const char *root)
{
    DIR *directory = opendir(root);
    if (!directory)
        return -1;
    int rc = 0;
    struct dirent *entry;
    while ((entry = readdir(directory))) {
        if (!profile_name_valid(entry->d_name))
            continue;
        if (stop_profile_for_uninstall(root, entry->d_name) != 0) {
            rc = -1;
            break;
        }
    }
    if (closedir(directory) != 0)
        rc = -1;
    return rc;
}

static int remove_directory(const char *path)
{
    const char *command[] = { "rm", "-rf", "--", path, NULL };
    return proc_run(command) == 0 ? 0 : -1;
}

static int remove_invocation_link(const struct uninstall_plan *plan)
{
    if (!plan->invocation_link_exists)
        return 0;
    char resolved[PATH_MAX];
    struct stat status;
    if (lstat(plan->invocation_link, &status) != 0 ||
        !S_ISLNK(status.st_mode) || !realpath(plan->invocation_link, resolved) ||
        strcmp(resolved, plan->executable) != 0 || unlink(plan->invocation_link) != 0) {
        logerr("cannot safely remove managed executable link: %s",
               plan->invocation_link);
        return -1;
    }
    return 0;
}

static int remove_safe_lock(const char *path, int exists)
{
    if (!exists)
        return 0;
    if (!safe_owned_regular(path, 0600) || unlink(path) != 0) {
        logerr("cannot safely remove installation lock: %s", path);
        return -1;
    }
    return 0;
}

int cmd_uninstall(int argc, char **argv)
{
    (void)argv;
    if (argc != 1) {
        fprintf(stderr, "usage: hamn uninstall\n");
        return 2;
    }
    struct uninstall_plan plan;
    memset(&plan, 0, sizeof(plan));
    if (discover_runtime(&plan) != 0 || discover_installation(&plan) != 0)
        return 1;
    if (plan.runtime_exists && plan.install_exists) {
        char runtime_real[PATH_MAX], install_real[PATH_MAX];
        if (!realpath(plan.runtime_root, runtime_real) ||
            !realpath(plan.install_root, install_real) ||
            path_is_within(runtime_real, install_real) ||
            path_is_within(install_real, runtime_real)) {
            logerr("refusing overlapping runtime and installation paths");
            return 1;
        }
    }
    if (!plan.runtime_exists && !plan.install_exists) {
        puts("nothing to uninstall");
        return 0;
    }
    print_plan(&plan);
    if (!confirm_uninstall())
        return 1;
    if (plan.runtime_exists && stop_all_profiles(plan.runtime_root) != 0)
        return 1;
    if (plan.runtime_exists && remove_directory(plan.runtime_root) != 0) {
        logerr("cannot remove Hamn runtime root: %s", plan.runtime_root);
        return 1;
    }
    if (remove_invocation_link(&plan) != 0)
        return 1;
    if (plan.install_exists && remove_directory(plan.install_root) != 0) {
        logerr("cannot remove managed installation root: %s",
               plan.install_root);
        return 1;
    }
    if (remove_safe_lock(plan.binary_lock, plan.binary_lock_exists) != 0 ||
        remove_safe_lock(plan.data_lock, plan.data_lock_exists) != 0)
        return 1;
    puts("Hamn has been uninstalled.");
    return 0;
}
