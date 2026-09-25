/* Runs the host's guest deployment recovery script against the real guest
 * transaction helper in a disposable root, with recorded systemctl. */
#include <assert.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "core/deployment_recovery.h"

#define HELPER "guest/scripts/guest-deployment-transaction.sh"
#define TOKEN "0123456789abcdef0123456789abcdef"

static char base[PATH_MAX], root[PATH_MAX], transactions[PATH_MAX];
static char hamnd[PATH_MAX], systemctl_log[PATH_MAX], path_env[PATH_MAX + 32];
static char error[8192];

/* Creates every missing directory above `path`. */
static void make_parents(const char *path)
{
    char parent[PATH_MAX];
    snprintf(parent, sizeof(parent), "%s", path);
    for (char *slash = strchr(parent + 1, '/'); slash; slash = strchr(slash + 1, '/')) {
        *slash = '\0';
        assert(mkdir(parent, 0755) == 0 || access(parent, F_OK) == 0);
        *slash = '/';
    }
}

static void make_transactions(void)
{
    make_parents(transactions);
    assert(mkdir(transactions, 0700) == 0);
}

static void write_file(const char *path, const char *text, mode_t mode)
{
    make_parents(path);
    FILE *file = fopen(path, "w");
    assert(file && fputs(text, file) >= 0 && fclose(file) == 0);
    assert(chmod(path, mode) == 0);
}

static void read_file(const char *path, char *text, size_t capacity)
{
    text[0] = '\0';
    int fd = open(path, O_RDONLY);
    if (fd < 0) return;
    ssize_t count = read(fd, text, capacity - 1);
    assert(count >= 0 && close(fd) == 0);
    text[count] = '\0';
}

/* Runs argv with the fixture PATH and root and `extra` (NAME=VALUE or NULL),
 * captures stderr in `error`, and returns the exit status. */
static int run(const char *const argv[], const char *extra)
{
    int pipes[2];
    assert(pipe(pipes) == 0);
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        dup2(pipes[1], STDERR_FILENO);
        int null = open("/dev/null", O_WRONLY);
        dup2(null, STDOUT_FILENO);
        close(pipes[0]);
        putenv(path_env);
        setenv("HAMN_DEPLOYMENT_TEST_ROOT", root, 1);
        setenv("SYSTEMCTL_LOG", systemctl_log, 1);
        if (extra) putenv((char *)extra);
        execvp(argv[0], (char *const *)argv);
        _exit(127);
    }
    close(pipes[1]);
    size_t length = 0;
    ssize_t count;
    while ((count = read(pipes[0], error + length, sizeof(error) - 1 - length)) > 0)
        length += (size_t)count;
    error[length] = '\0';
    close(pipes[0]);
    int status;
    assert(waitpid(child, &status, 0) == child && WIFEXITED(status));
    return WEXITSTATUS(status);
}

static int recover(const char *extra)
{
    write_file(systemctl_log, "", 0600);
    const char *argv[] = { "bash", "-c", DEPLOYMENT_RECOVERY_SCRIPT, "--",
        transactions, HELPER, NULL };
    return run(argv, extra);
}

static void begin(const char *token)
{
    const char *argv[] = { "bash", HELPER, "begin", token, NULL };
    assert(run(argv, NULL) == 0);
}

static void assert_hamnd(const char *expected)
{
    char text[64];
    read_file(hamnd, text, sizeof(text));
    assert(!strcmp(text, expected));
}

static int exists(const char *name)
{
    char path[PATH_MAX];
    snprintf(path, sizeof(path), "%s/%s", transactions, name);
    struct stat status;
    return lstat(path, &status) == 0;
}

static void assert_no_systemctl(void)
{
    char text[64];
    read_file(systemctl_log, text, sizeof(text));
    assert(text[0] == '\0');
}

static void reset(void)
{
    char command[PATH_MAX + 16];
    snprintf(command, sizeof(command), "rm -rf '%s'", root);
    assert(system(command) == 0);
    assert(mkdir(root, 0700) == 0);
    write_file(hamnd, "old", 0755);
}

int main(void)
{
    char temporary[] = "/tmp/hamn-deployment-recovery-XXXXXX";
    assert(mkdtemp(temporary) && realpath(temporary, base));
    snprintf(root, sizeof(root), "%s/root", base);
    snprintf(transactions, sizeof(transactions), "%s/var/lib/hamn/deployment-transactions", root);
    snprintf(hamnd, sizeof(hamnd), "%s/usr/local/bin/hamnd", root);
    snprintf(systemctl_log, sizeof(systemctl_log), "%s/systemctl.log", base);
    snprintf(path_env, sizeof(path_env), "PATH=%s/bin:/usr/bin:/bin", base);
    char fixture[PATH_MAX];
    snprintf(fixture, sizeof(fixture), "%s/bin/systemctl", base);
    write_file(fixture,
        "#!/bin/bash\nprintf '%s\\n' \"$*\" >>\"$SYSTEMCTL_LOG\"\n"
        "case $1 in\n"
        "is-enabled) echo enabled ;;\n"
        "reset-failed) exit \"${FAIL_RESET_FAILED:-0}\" ;;\n"
        "restart) exit \"${FAIL_RESTART:-0}\" ;;\n"
        "esac\nexit 0\n", 0755);
    snprintf(fixture, sizeof(fixture), "%s/bin/sysctl", base);
    write_file(fixture, "#!/bin/bash\nexit 0\n", 0755);

    /* No transaction root, or an empty one: nothing to recover. */
    reset();
    assert(recover(NULL) == 0);
    assert_no_systemctl();
    make_transactions();
    assert(recover(NULL) == 0);
    assert_no_systemctl();

    /* A ready backup is restored after clearing systemd's start limit, then
     * removed; a second recovery finds nothing. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(recover(NULL) == 0);
    assert_hamnd("old");
    assert(!exists(TOKEN));
    char calls[4096];
    read_file(systemctl_log, calls, sizeof(calls));
    assert(!strncmp(calls, "reset-failed docker.service docker.socket containerd.service "
                           "hamnd.service hamn-host-dns.service\n", 91));
    assert(strstr(calls, "daemon-reload"));
    assert(recover(NULL) == 0);
    assert_no_systemctl();

    /* systemd state cannot be cleared: nothing is restored, the backup stays,
     * and a later attempt completes the recovery. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(recover("FAIL_RESET_FAILED=1") != 0);
    assert_hamnd("new");
    assert(exists(TOKEN));
    read_file(systemctl_log, calls, sizeof(calls));
    assert(!strstr(calls, "daemon-reload"));
    assert(recover(NULL) == 0);
    assert_hamnd("old");
    assert(!exists(TOKEN));

    /* A failed rollback keeps its backup for the next attempt. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(recover("FAIL_RESTART=1") != 0);
    assert(strstr(error, "backup retained"));
    assert(exists(TOKEN));
    assert(recover(NULL) == 0);
    assert_hamnd("old");
    assert(!exists(TOKEN));

    /* More than one entry: none is chosen. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    char other[PATH_MAX];
    snprintf(other, sizeof(other), "%s/1123456789abcdef0123456789abcdef", transactions);
    assert(mkdir(other, 0700) == 0);
    assert(recover(NULL) == 1 && strstr(error, "multiple deployment backups"));
    assert_no_systemctl();
    assert_hamnd("new");
    assert(exists(TOKEN));

    /* An incomplete backup is never restored. */
    reset();
    make_transactions();
    assert(mkdir(other, 0700) == 0);
    assert(recover(NULL) == 1 && strstr(error, "incomplete deployment backup"));
    char phase[PATH_MAX + 8];
    snprintf(phase, sizeof(phase), "%s/phase", other);
    write_file(phase, "begin\n", 0600);
    assert(recover(NULL) == 1 && strstr(error, "incomplete deployment backup"));
    assert(unlink(phase) == 0 && mkdir(phase, 0700) == 0);
    assert(recover(NULL) == 1 && strstr(error, "incomplete deployment backup"));
    assert_no_systemctl();
    assert(exists("1123456789abcdef0123456789abcdef"));

    /* Entries that are not owned 32-hex directories are reported. */
    const char *invalid[] = { "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcde", ".hidden", NULL };
    for (size_t index = 0; invalid[index]; index++) {
        reset();
        char entry[PATH_MAX];
        snprintf(entry, sizeof(entry), "%s/%s", transactions, invalid[index]);
        make_transactions();
        assert(mkdir(entry, 0700) == 0);
        assert(recover(NULL) == 1 && strstr(error, "invalid deployment backup"));
        assert_no_systemctl();
    }
    reset();
    begin(TOKEN);
    char moved[PATH_MAX];
    snprintf(moved, sizeof(moved), "%s/elsewhere", root);
    snprintf(other, sizeof(other), "%s/%s", transactions, TOKEN);
    assert(rename(other, moved) == 0 && symlink(moved, other) == 0);
    assert(recover(NULL) == 1 && strstr(error, "invalid deployment backup"));
    assert_no_systemctl();

    /* A symlinked transaction root is refused. */
    reset();
    snprintf(moved, sizeof(moved), "%s/real-transactions", root);
    assert(mkdir(moved, 0700) == 0);
    write_file(hamnd, "old", 0755);
    make_parents(transactions);
    assert(symlink(moved, transactions) == 0);
    assert(recover(NULL) == 1 && strstr(error, "unsafe deployment backup root"));
    assert_no_systemctl();

    char command[PATH_MAX + 16];
    snprintf(command, sizeof(command), "rm -rf '%s'", base);
    assert(system(command) == 0);
    puts("PASS: deployment recovery rolls back only a single ready backup and retains it on failure");
    return 0;
}
