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

static char base[PATH_MAX], root[PATH_MAX], transactions[PATH_MAX], record[PATH_MAX];
static char hamnd[PATH_MAX], systemctl_log[PATH_MAX], sync_log[PATH_MAX];
static char path_env[PATH_MAX + 32];
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
        setenv("SYNC_LOG", sync_log, 1);
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

/* Runs recovery with `helper` and the NULL-terminated digests of helpers
 * that published without fsync; `extra` is NAME=VALUE or NULL. */
static int recover_with(const char *helper, const char *const digests[],
                        const char *extra)
{
    write_file(systemctl_log, "", 0600);
    write_file(sync_log, "", 0600);
    const char *argv[16] = { "bash", "-c", DEPLOYMENT_RECOVERY_SCRIPT, "--",
        transactions, helper, record };
    size_t count = 7;
    for (size_t index = 0; digests[index]; index++) {
        assert(count + 1 < sizeof(argv) / sizeof(argv[0]));
        argv[count++] = digests[index];
    }
    argv[count] = NULL;
    return run(argv, extra);
}

/* Recovery as the host runs it: the current helper and the real list. */
static int recover(const char *extra)
{
    static const char *const undurable[] = { DEPLOYMENT_UNDURABLE_HELPERS, NULL };
    return recover_with(HELPER, undurable, extra);
}

/* The SHA-256 of the helper under test, as lowercase hex. */
static void helper_digest(char digest[65])
{
    FILE *pipe = popen("/usr/bin/shasum -a 256 " HELPER, "r");
    assert(pipe && fread(digest, 1, 64, pipe) == 64 && pclose(pipe) == 0);
    digest[64] = '\0';
    assert(strspn(digest, "0123456789abcdef") == 64);
}

/* Runs the helper's begin; `extra` is FAIL_SYNC_AT=... or NULL. */
static int begin_with(const char *token, const char *extra)
{
    write_file(sync_log, "", 0600);
    const char *argv[] = { "bash", HELPER, "begin", token, NULL };
    return run(argv, extra);
}

static void begin(const char *token)
{
    assert(begin_with(token, NULL) == 0);
}

/* Removes the entry of `token`, as an operator would after inspecting it. */
static void remove_entry(const char *token)
{
    char command[PATH_MAX + 64];
    snprintf(command, sizeof(command), "rm -rf '%s/%s'", transactions, token);
    assert(system(command) == 0);
}

/* Runs the helper's commit; `extra` is FAIL_SYNC_AT=N or NULL. */
static int commit(const char *token, const char *extra)
{
    write_file(sync_log, "", 0600);
    const char *argv[] = { "bash", HELPER, "commit", token, NULL };
    return run(argv, extra);
}

/* Replaces the phase file of `token` with `size` bytes of `bytes`. */
static void write_phase(const char *token, const char *bytes, size_t size)
{
    char path[PATH_MAX];
    snprintf(path, sizeof(path), "%s/%s/phase", transactions, token);
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    assert(fd >= 0 && write(fd, bytes, size) == (ssize_t)size && close(fd) == 0);
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

static int record_exists(void)
{
    struct stat status;
    return lstat(record, &status) == 0;
}

/* The host's part: a committed whole runtime removes the record. */
static void settle(void)
{
    assert(unlink(record) == 0);
}

/* Before the evidence went, the record was created and made durable: the
 * first sync(1) call names the record and its directory. */
static void assert_recorded_first(void)
{
    assert(record_exists());
    char calls[PATH_MAX * 2 + 16], first[PATH_MAX * 2 + 16];
    read_file(sync_log, calls, sizeof(calls));
    char directory[PATH_MAX];
    snprintf(directory, sizeof(directory), "%s", record);
    *strrchr(directory, '/') = '\0';
    snprintf(first, sizeof(first), "-- %s %s\n", record, directory);
    assert(!strncmp(calls, first, strlen(first)));
}

/* The entry was discarded durably and named, and nothing was restored. */
static void assert_discarded(const char *token, const char *state)
{
    assert_recorded_first();
    char expected[128];
    snprintf(expected, sizeof(expected),
             "discarded deployment backup %s (phase %s)", token, state);
    assert(strstr(error, expected) && strstr(error, "never restored"));
    assert(!strstr(error, "null byte"));
    assert(!exists(token));
    assert_no_systemctl();
    char calls[PATH_MAX + 16], last[PATH_MAX + 16];
    read_file(sync_log, calls, sizeof(calls));
    snprintf(last, sizeof(last), "-- %s\n", transactions);
    size_t length = strlen(calls);
    assert(length >= strlen(last) &&
           !strcmp(calls + length - strlen(last), last));
}

/* Recovery prints nothing only when there was no backup and no record: the
 * host takes any output as a recovered backup and applies the whole runtime
 * again. */
static void assert_nothing_recovered(void)
{
    assert(error[0] == '\0');
    assert(!record_exists());
    assert_no_systemctl();
}

/* The ready backup was restored and removed, and one line names it. */
static void assert_rolled_back(const char *token)
{
    char expected[128];
    snprintf(expected, sizeof(expected), "rolled back deployment backup %s: ",
             token);
    assert(strstr(error, expected) && strstr(error, "before its transaction"));
    assert(!exists(token));
    assert_recorded_first();
}

/* No backup, but the record of an earlier recovery: reported, and kept. */
static void assert_pending(void)
{
    assert(strstr(error, "still to be applied again after an earlier "
                         "deployment recovery"));
    assert(record_exists());
    assert_no_systemctl();
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
    snprintf(record, sizeof(record), "%s/var/lib/hamn/deployment-reapply", root);
    snprintf(hamnd, sizeof(hamnd), "%s/usr/local/bin/hamnd", root);
    snprintf(systemctl_log, sizeof(systemctl_log), "%s/systemctl.log", base);
    snprintf(sync_log, sizeof(sync_log), "%s/sync.log", base);
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
    /* One line of operands per sync(1) call; FAIL_SYNC_AT="N [M ...]" fails
     * the Nth (and Mth) call. */
    snprintf(fixture, sizeof(fixture), "%s/bin/sync", base);
    write_file(fixture,
        "#!/bin/bash\nprintf '%s\\n' \"$*\" >>\"$SYNC_LOG\"\n"
        "call=$(wc -l <\"$SYNC_LOG\" | tr -d ' ')\n"
        "case \" ${FAIL_SYNC_AT:-} \" in *\" $call \"*) "
        "echo 'injected durability failure' >&2; exit 1 ;; esac\n", 0755);
    /* The guest's coreutils digest; macOS ships shasum. */
    snprintf(fixture, sizeof(fixture), "%s/bin/sha256sum", base);
    write_file(fixture, "#!/bin/bash\nexec /usr/bin/shasum -a 256 \"$@\"\n", 0755);

    /* The undurable helpers are two well-formed digests, and the helper
     * under test is not one of them. */
    char digest[65];
    helper_digest(digest);
    static const char *const undurable[] = { DEPLOYMENT_UNDURABLE_HELPERS };
    assert(sizeof(undurable) / sizeof(undurable[0]) == 2);
    for (size_t index = 0; index < 2; index++)
        assert(strlen(undurable[index]) == 64 &&
               strspn(undurable[index], "0123456789abcdef") == 64 &&
               strcmp(undurable[index], digest) != 0);

    /* No transaction root, or an empty one: nothing to recover. */
    reset();
    assert(recover(NULL) == 0);
    assert_nothing_recovered();
    make_transactions();
    assert(recover(NULL) == 0);
    assert_nothing_recovered();

    /* A ready backup is restored after clearing systemd's start limit, then
     * removed and named. Its record outlives it: every later recovery
     * reports it until the host removes it after a committed whole runtime,
     * and then nothing is found. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(recover(NULL) == 0);
    assert_hamnd("old");
    assert_rolled_back(TOKEN);
    char calls[4096];
    read_file(systemctl_log, calls, sizeof(calls));
    assert(!strncmp(calls, "reset-failed docker.service docker.socket containerd.service "
                           "hamnd.service hamn-host-dns.service\n", 91));
    assert(strstr(calls, "daemon-reload"));
    assert(recover(NULL) == 0);
    assert_pending();
    assert(recover(NULL) == 0);
    assert_pending();
    settle();
    assert(recover(NULL) == 0);
    assert_nothing_recovered();

    /* A record that is not a regular file is refused before anything else:
     * the ready backup is kept and nothing is restored. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(symlink("elsewhere", record) == 0);
    assert(recover(NULL) == 1 && strstr(error, "unsafe deployment reapply record"));
    assert_no_systemctl();
    assert_hamnd("new");
    assert(exists(TOKEN));
    assert(unlink(record) == 0 && mkdir(record, 0700) == 0);
    assert(recover(NULL) == 1 && strstr(error, "unsafe deployment reapply record"));
    assert(rmdir(record) == 0);
    assert_hamnd("new");
    assert(exists(TOKEN));

    /* A record that cannot be made durable removes no evidence: the ready
     * backup stays and is not restored; a later attempt restores it. */
    assert(recover("FAIL_SYNC_AT=1") == 1 &&
           strstr(error, "cannot record that the whole guest runtime"));
    assert_no_systemctl();
    assert_hamnd("new");
    assert(exists(TOKEN));
    assert(recover(NULL) == 0);
    assert_hamnd("old");
    assert_rolled_back(TOKEN);
    settle();

    /* systemd state cannot be cleared: nothing is restored, the backup stays,
     * and a later attempt completes the recovery. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(recover("FAIL_RESET_FAILED=1") != 0);
    assert_hamnd("new");
    assert(exists(TOKEN));
    read_file(systemctl_log, calls, sizeof(calls));
    assert(!strstr(calls, "daemon-reload"));
    assert(!strstr(error, "rolled back"));
    assert(recover(NULL) == 0);
    assert_hamnd("old");
    assert_rolled_back(TOKEN);

    /* A failed rollback keeps its backup for the next attempt. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(recover("FAIL_RESTART=1") != 0);
    assert(strstr(error, "backup retained") && !strstr(error, "rolled back"));
    assert(exists(TOKEN));
    assert(recover(NULL) == 0);
    assert_hamnd("old");
    assert_rolled_back(TOKEN);

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

    /* A backup whose phase is not ready is never restored. Its begin never
     * returned, so the guest is unchanged, or commit or rollback had already
     * decided its outcome: it is discarded durably and named, and recovery
     * succeeds. An entry begin left before writing anything: */
    reset();
    make_transactions();
    assert(mkdir(other, 0700) == 0);
    assert(recover(NULL) == 0);
    assert_discarded("1123456789abcdef0123456789abcdef", "missing");

    /* A complete backup whose phase a crash left zero-filled: its removal
     * was lost after the freed block was discarded (observed after an ext4
     * shutdown lost the commit's removal). The deployed runtime stays. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    write_phase(TOKEN, "\0\0\0\0\0\0", 6);
    assert(recover(NULL) == 0);
    assert_discarded(TOKEN, "zero-filled");
    assert_hamnd("new");

    /* Any other bytes no helper writes since 0.1.1 (empty, foreign, a
     * trailing NUL or newline, or too few zeros): they cannot show the guest
     * was left unchanged, so the backup is kept and neither restored nor
     * discarded, and the message names the entry. */
    static const struct { const char *bytes; size_t size; } damaged[] = {
        { "", 0 }, { "begin\n", 6 }, { "ready\0", 6 }, { "ready\n\n", 7 },
        { "\0\0\0\0\0", 5 },
    };
    for (size_t index = 0; index < sizeof(damaged) / sizeof(damaged[0]); index++) {
        write_file(hamnd, "old", 0755);
        begin(TOKEN);
        write_file(hamnd, "new", 0755);
        write_phase(TOKEN, damaged[index].bytes, damaged[index].size);
        assert(recover(NULL) == 1 &&
               strstr(error, "damaged deployment backup phase; neither restored "
                             "nor discarded: inspect") &&
               strstr(error, TOKEN));
        assert(!strstr(error, "null byte"));
        assert_no_systemctl();
        assert_hamnd("new");
        assert(exists(TOKEN));
        remove_entry(TOKEN);
    }
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    char phase[PATH_MAX + 8];
    snprintf(phase, sizeof(phase), "%s/%s/phase", transactions, TOKEN);
    assert(unlink(phase) == 0);
    assert(recover(NULL) == 0);
    assert_discarded(TOKEN, "missing");
    assert_hamnd("new");

    /* The 0.0.1 and 0.1.0 helpers wrote phase in place without fsync, so
     * for them a missing or zero-filled phase can follow applied changes:
     * the backup is kept and refused as ambiguous, neither restored nor
     * discarded. Its ready backup still rolls back. The helper under test
     * stands in for one of them here. */
    const char *const old_helper[] = { DEPLOYMENT_UNDURABLE_HELPERS, digest, NULL };
    write_file(hamnd, "old", 0755);
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    write_phase(TOKEN, "\0\0\0\0\0\0", 6);
    assert(recover_with(HELPER, old_helper, NULL) == 1 &&
           strstr(error, "ambiguous deployment backup (phase zero-filled)") &&
           strstr(error, TOKEN));
    assert(unlink(phase) == 0);
    assert(recover_with(HELPER, old_helper, NULL) == 1 &&
           strstr(error, "ambiguous deployment backup (phase missing)"));
    assert_no_systemctl();
    assert_hamnd("new");
    assert(exists(TOKEN));
    write_phase(TOKEN, "ready\n", 6);
    assert(recover_with(HELPER, old_helper, NULL) == 0);
    assert_hamnd("old");
    assert_rolled_back(TOKEN);

    /* Without its helper, a phase that is not ready cannot be judged. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(unlink(phase) == 0);
    char no_helper[PATH_MAX];
    snprintf(no_helper, sizeof(no_helper), "%s/no-such-helper", base);
    const char *const none[] = { NULL };
    assert(recover_with(no_helper, none, NULL) == 1 &&
           strstr(error, "cannot identify the deployment helper"));
    assert_no_systemctl();
    assert_hamnd("new");
    assert(exists(TOKEN));
    remove_entry(TOKEN);

    /* An interrupted commit. Before its runtime (the protected items, then
     * the root filesystem) is durable the backup stays ready and intact, and
     * recovery restores it. Once the phase is retired, recovery never
     * restores it: it discards what is left, or finds nothing. */
    static const char *const runtime_barriers[] = {
        "FAIL_SYNC_AT=1", "FAIL_SYNC_AT=2",
    };
    for (size_t index = 0; index < 2; index++) {
        write_file(hamnd, "old", 0755);
        begin(TOKEN);
        write_file(hamnd, "new", 0755);
        assert(commit(TOKEN, runtime_barriers[index]) != 0 &&
               strstr(error, "backup retained"));
        assert(recover(NULL) == 0);
        assert_hamnd("old");
        assert_rolled_back(TOKEN);
    }
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(commit(TOKEN, "FAIL_SYNC_AT=3") != 0 &&
           strstr(error, "but cannot make that durable"));
    assert(exists(TOKEN));
    assert(recover(NULL) == 0);
    assert_discarded(TOKEN, "missing");
    assert_hamnd("new");
    write_file(hamnd, "old", 0755);
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(commit(TOKEN, "FAIL_SYNC_AT=4") != 0 &&
           strstr(error, "cannot make the removal durable"));
    assert(!exists(TOKEN));
    settle(); /* the record of the discards above */
    assert(recover(NULL) == 0);
    assert_nothing_recovered();
    assert_hamnd("new");

    /* A begin whose last barrier fails, and whose phase removal cannot be
     * made durable either, leaves its backup without a phase. The guest was
     * never changed, and recovery discards the entry. */
    write_file(hamnd, "old", 0755);
    assert(begin_with(TOKEN, "FAIL_SYNC_AT=3 4") != 0 &&
           strstr(error, "for deployment recovery"));
    assert(exists(TOKEN));
    assert(recover(NULL) == 0);
    assert_discarded(TOKEN, "missing");
    assert_hamnd("old");

    /* A phase that cannot be read (here: no read permission) may belong to a
     * ready backup: it is neither discarded nor restored. Once it reads
     * again, the intact backup rolls back. */
    write_file(hamnd, "old", 0755);
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(chmod(phase, 0) == 0);
    assert(recover(NULL) == 1 && strstr(error, "cannot read deployment backup phase"));
    assert_no_systemctl();
    assert_hamnd("new");
    assert(exists(TOKEN));
    assert(chmod(phase, 0600) == 0);
    assert(recover(NULL) == 0);
    assert_hamnd("old");
    assert_rolled_back(TOKEN);

    /* A phase that is a directory or a symlink is not a state any helper
     * writes: it is refused and kept, and nothing is restored. */
    begin(TOKEN);
    write_file(hamnd, "new", 0755);
    assert(unlink(phase) == 0 && mkdir(phase, 0700) == 0);
    assert(recover(NULL) == 1 && strstr(error, "unsafe deployment backup phase"));
    assert(rmdir(phase) == 0 && symlink("ready", phase) == 0);
    assert(recover(NULL) == 1 && strstr(error, "unsafe deployment backup phase"));
    assert_no_systemctl();
    assert_hamnd("new");
    assert(exists(TOKEN));

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
    puts("PASS: deployment recovery rolls back only a single ready backup, "
         "retains it on failure, discards one whose phase is missing or "
         "zero-filled, keeps one whose phase is damaged and records durably "
         "that the whole runtime must be applied again");
    return 0;
}
