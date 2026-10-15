/* The guest root filesystem check (core/guest_filesystem.h): its probe
 * script run by bash against fixture /proc and /sys trees, the
 * classification of what the SSH client reports, the probe through a
 * stand-in ssh that runs the command it is given as sshd runs it, and the
 * profile record that `vm status` reports. */
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/control.h"
#include "core/guest_filesystem.h"
#include "core/profile.h"
#include "util/proc.h"

static char base[PATH_MAX];

static void fail(const char *format, ...) __attribute__((format(printf, 1, 2), noreturn));

static void fail(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    fputs("FAIL: ", stderr);
    vfprintf(stderr, format, arguments);
    fputc('\n', stderr);
    va_end(arguments);
    exit(1);
}

#define REQUIRE(condition, ...) \
    do { if (!(condition)) fail(__VA_ARGS__); } while (0)

static void write_file(const char *path, const char *text, mode_t mode)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, mode);
    REQUIRE(fd >= 0, "open %s: %s", path, strerror(errno));
    size_t length = strlen(text);
    REQUIRE(write(fd, text, length) == (ssize_t)length && close(fd) == 0,
            "write %s", path);
    REQUIRE(chmod(path, mode) == 0, "chmod %s", path);
}

static void make_directory(const char *path)
{
    REQUIRE(mkdir(path, 0700) == 0 || errno == EEXIST, "mkdir %s: %s", path,
            strerror(errno));
}

/* Runs argv with stdout and stderr in output; returns the exit status. */
static int run(const char *const argv[], const char *path_env, char *output,
               size_t capacity)
{
    int pipes[2];
    REQUIRE(pipe(pipes) == 0, "pipe");
    pid_t child = fork();
    REQUIRE(child >= 0, "fork");
    if (child == 0) {
        dup2(pipes[1], STDOUT_FILENO);
        dup2(pipes[1], STDERR_FILENO);
        close(pipes[0]);
        close(pipes[1]);
        if (path_env)
            setenv("PATH", path_env, 1);
        execvp(argv[0], (char *const *)argv);
        _exit(127);
    }
    close(pipes[1]);
    size_t length = 0;
    ssize_t count;
    while ((count = read(pipes[0], output + length,
                         capacity - 1 - length)) > 0)
        length += (size_t)count;
    output[length] = '\0';
    close(pipes[0]);
    int status;
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status),
            "%s did not exit", argv[0]);
    return WEXITSTATUS(status);
}

/* A guest's /proc/self/mountinfo, /sys/dev/block and /sys/fs/ext4. */
struct fixture {
    char root[PATH_MAX], mountinfo[PATH_MAX], devices[PATH_MAX];
    char ext4[PATH_MAX];
};

static void fixture_init(struct fixture *f, const char *name)
{
    snprintf(f->root, sizeof(f->root), "%s/%s", base, name);
    snprintf(f->mountinfo, sizeof(f->mountinfo), "%s/mountinfo", f->root);
    snprintf(f->devices, sizeof(f->devices), "%s/devices", f->root);
    snprintf(f->ext4, sizeof(f->ext4), "%s/ext4", f->root);
    char block[PATH_MAX];
    snprintf(block, sizeof(block), "%s/block", f->root);
    make_directory(f->root);
    make_directory(f->devices);
    make_directory(f->ext4);
    make_directory(block);
}

/* A block device devno named name; its ext4 record when count is set. */
static void fixture_device(struct fixture *f, const char *devno,
                           const char *name, const char *count,
                           const char *first, const char *last)
{
    char path[PATH_MAX], target[PATH_MAX];
    snprintf(path, sizeof(path), "%s/block/%s", f->root, name);
    make_directory(path);
    snprintf(path, sizeof(path), "%s/%s", f->devices, devno);
    snprintf(target, sizeof(target), "../block/%s", name);
    REQUIRE(symlink(target, path) == 0, "symlink %s", path);
    if (!count)
        return;
    snprintf(path, sizeof(path), "%s/%s", f->ext4, name);
    make_directory(path);
    const char *files[][2] = { { "errors_count", count },
        { "first_error_time", first }, { "last_error_time", last } };
    for (size_t index = 0; index < 3; index++) {
        char file[PATH_MAX], text[64];
        snprintf(file, sizeof(file), "%s/%s", path, files[index][0]);
        snprintf(text, sizeof(text), "%s\n", files[index][1]);
        write_file(file, text, 0644);
    }
}

#define ROOT_LINE "29 1 253:1 / / rw,relatime shared:1 - ext4 /dev/vda1 " \
    "rw,errors=remount-ro\n"
#define PROC_LINE "22 1 0:21 / /proc rw,nosuid,nodev shared:12 - proc proc rw\n"
#define BOOT_LINE "30 29 253:15 / /boot/efi rw,relatime shared:3 - vfat " \
    "/dev/vda15 rw,fmask=0077\n"

static const char *interpreters[2];
static size_t interpreter_count;

static void choose_interpreters(void)
{
    /* The guest runs bash 5; macOS ships bash 3.2 as /bin/bash. The script
     * must work in both, so run it in each bash this host has. */
    interpreters[interpreter_count++] = "/bin/bash";
    char output[PATH_MAX];
    const char *const which[] = { "/bin/sh", "-c", "command -v bash", NULL };
    if (run(which, NULL, output, sizeof(output)) == 0) {
        output[strcspn(output, "\n")] = '\0';
        char real_path[PATH_MAX], real_bin[PATH_MAX];
        if (realpath(output, real_path) && realpath("/bin/bash", real_bin) &&
            strcmp(real_path, real_bin) != 0)
            interpreters[interpreter_count++] = strdup(output);
    }
}

struct script_case {
    const char *name;
    int status;
    const char *output; /* exact, or a prefix when status is not 0 */
    enum guest_filesystem_state state;
};

static void probe_script_case(const struct fixture *f,
                              const struct script_case *expected)
{
    for (size_t index = 0; index < interpreter_count; index++) {
        const char *const argv[] = { interpreters[index], "-c",
            GUEST_FILESYSTEM_PROBE_SCRIPT, "--", f->mountinfo, f->devices,
            f->ext4, NULL };
        char output[4096];
        int status = run(argv, NULL, output, sizeof(output));
        REQUIRE(status == expected->status, "%s (%s): status %d, output %s",
                expected->name, interpreters[index], status, output);
        if (expected->status == 0)
            REQUIRE(strcmp(output, expected->output) == 0,
                    "%s (%s): output %s", expected->name,
                    interpreters[index], output);
        else
            REQUIRE(strstr(output, expected->output) != NULL,
                    "%s (%s): output %s", expected->name,
                    interpreters[index], output);
        struct guest_filesystem_check check;
        guest_filesystem_classify(status, output, 0, 0, &check);
        REQUIRE(check.state == expected->state, "%s (%s): state %d (%s)",
                expected->name, interpreters[index], check.state,
                check.detail);
    }
}

static void probe_script_reports_the_root_filesystem_and_its_ext4_record(void)
{
    struct fixture f;
    fixture_init(&f, "healthy");
    write_file(f.mountinfo, PROC_LINE ROOT_LINE BOOT_LINE, 0644);
    fixture_device(&f, "253:1", "vda1", "0", "0", "0");
    fixture_device(&f, "253:15", "vda15", NULL, NULL, NULL);
    struct script_case healthy = { "healthy", 0,
        "root 253:1 ext4 rw,relatime rw,errors=remount-ro\nerrors 0 0 0\n",
        GUEST_FILESYSTEM_HEALTHY };
    probe_script_case(&f, &healthy);

    fixture_init(&f, "recorded");
    write_file(f.mountinfo, ROOT_LINE, 0644);
    fixture_device(&f, "253:1", "vda1", "3", "1790000000", "1790000100");
    struct script_case recorded = { "errors recorded", 0,
        "root 253:1 ext4 rw,relatime rw,errors=remount-ro\n"
        "errors 3 1790000000 1790000100\n", GUEST_FILESYSTEM_ERRORS_RECORDED };
    probe_script_case(&f, &recorded);

    /* What the kernel shows after errors=remount-ro took effect. */
    fixture_init(&f, "read-only");
    write_file(f.mountinfo, "29 1 253:1 / / ro,relatime - ext4 /dev/vda1 "
               "ro,errors=remount-ro\n", 0644);
    fixture_device(&f, "253:1", "vda1", "0", "0", "0");
    struct script_case read_only = { "read-only", 0,
        "root 253:1 ext4 ro,relatime ro,errors=remount-ro\nerrors 0 0 0\n",
        GUEST_FILESYSTEM_READ_ONLY };
    probe_script_case(&f, &read_only);

    /* The last mount at / covers the earlier one; no optional fields. */
    fixture_init(&f, "overmounted");
    write_file(f.mountinfo, ROOT_LINE PROC_LINE
               "40 29 253:2 / / rw shared:9 master:2 propagate_from:1 - ext4 "
               "/dev/vdb rw\n", 0644);
    fixture_device(&f, "253:1", "vda1", "5", "1", "2");
    fixture_device(&f, "253:2", "vdb", "0", "0", "0");
    struct script_case overmounted = { "overmounted", 0,
        "root 253:2 ext4 rw rw\nerrors 0 0 0\n", GUEST_FILESYSTEM_HEALTHY };
    probe_script_case(&f, &overmounted);

    /* A root that is not ext4 has no record to read. */
    fixture_init(&f, "btrfs");
    write_file(f.mountinfo, "29 1 0:30 /@ / rw,relatime - btrfs /dev/vda1 "
               "rw,space_cache=v2\n", 0644);
    struct script_case btrfs = { "btrfs", 0,
        "root 0:30 btrfs rw,relatime rw,space_cache=v2\n",
        GUEST_FILESYSTEM_HEALTHY };
    probe_script_case(&f, &btrfs);

    /* A path that only begins with / is not the root, and a root line
     * without the separator before its type is not one. */
    fixture_init(&f, "no-root");
    write_file(f.mountinfo, PROC_LINE BOOT_LINE
               "31 1 253:1 / / rw,relatime shared:1 ext4 /dev/vda1 rw\n",
               0644);
    struct script_case no_root = { "no root", 3, "no root mount in ",
        GUEST_FILESYSTEM_UNKNOWN };
    probe_script_case(&f, &no_root);

    fixture_init(&f, "no-mountinfo");
    struct script_case no_mountinfo = { "no mountinfo", 3,
        "No such file or directory", GUEST_FILESYSTEM_UNKNOWN };
    probe_script_case(&f, &no_mountinfo);

    fixture_init(&f, "no-device");
    write_file(f.mountinfo, ROOT_LINE, 0644);
    struct script_case no_device = { "no device", 4,
        "root 253:1 ext4 rw,relatime rw,errors=remount-ro\n",
        GUEST_FILESYSTEM_UNKNOWN };
    probe_script_case(&f, &no_device);

    fixture_init(&f, "no-record");
    write_file(f.mountinfo, ROOT_LINE, 0644);
    fixture_device(&f, "253:1", "vda1", NULL, NULL, NULL);
    struct script_case no_record = { "no ext4 record", 4,
        "errors_count", GUEST_FILESYSTEM_UNKNOWN };
    probe_script_case(&f, &no_record);
}

struct classification {
    int rc, timed_out, cancelled;
    const char *output;
    enum guest_filesystem_state state;
    const char *detail; /* a part of the detail */
};

#define HEALTHY_OUTPUT "root 253:1 ext4 rw,relatime rw,errors=remount-ro\nerrors 0 0 0"

static void classification_needs_exact_evidence(void)
{
    static const struct classification cases[] = {
        { 0, 0, 0, HEALTHY_OUTPUT, GUEST_FILESYSTEM_HEALTHY, "no errors" },
        { 0, 0, 0, HEALTHY_OUTPUT "\n\n", GUEST_FILESYSTEM_HEALTHY,
          "no errors" },
        { 0, 0, 0, "root 253:1 ext4 rw rw\nerrors 1 1790000000 1790000100",
          GUEST_FILESYSTEM_ERRORS_RECORDED,
          "ext4 recorded 1 error since its last complete check, the first at "
          "2026-09-21T14:13:20Z and the last at 2026-09-21T14:15:00Z" },
        { 0, 0, 0, "root 253:1 ext4 rw rw\nerrors 4294967295 0 0",
          GUEST_FILESYSTEM_ERRORS_RECORDED,
          "4294967295 errors since its last complete check, the first at an "
          "unrecorded time and the last at an unrecorded time" },
        /* Recorded errors explain a read-only root better than ro does. */
        { 0, 0, 0, "root 253:1 ext4 ro ro,errors=remount-ro\nerrors 2 5 6",
          GUEST_FILESYSTEM_ERRORS_RECORDED, "recorded 2 errors" },
        { 0, 0, 0, "root 253:1 ext4 rw ro,errors=remount-ro\nerrors 0 0 0",
          GUEST_FILESYSTEM_READ_ONLY,
          "mounted read-only (ro,errors=remount-ro)" },
        { 0, 0, 0, "root 253:1 ext4 ro,relatime rw\nerrors 0 0 0",
          GUEST_FILESYSTEM_READ_ONLY, "read-only" },
        { 0, 0, 0, "root 0:30 btrfs rw rw", GUEST_FILESYSTEM_HEALTHY,
          "btrfs" },
        /* What sshd prints when it cannot execute the login shell, and
         * what it prints before running one in an unreadable home. */
        { 1, 0, 0, "/bin/bash: Input/output error",
          GUEST_FILESYSTEM_UNREADABLE,
          "the guest cannot read its disk: /bin/bash: Input/output error" },
        { 0, 0, 0, "Could not chdir to home directory /home/hamn: "
          "Input/output error\n" HEALTHY_OUTPUT, GUEST_FILESYSTEM_UNREADABLE,
          "/home/hamn: Input/output error" },
        { 126, 0, 0, "bash: /usr/bin/x\x1b[2J: Input/output error",
          GUEST_FILESYSTEM_UNREADABLE, "/usr/bin/x?[2J: Input/output error" },
        /* 255 is the SSH client's own failure, not the guest's. */
        { 255, 0, 0, "mux_client_request_session: read from master failed: "
          "Input/output error", GUEST_FILESYSTEM_UNKNOWN, "ssh failed: " },
        { 255, 0, 0, "ssh: connect to host 192.0.2.1 port 22: Connection "
          "refused", GUEST_FILESYSTEM_UNKNOWN, "Connection refused" },
        { -1, 0, 0, "", GUEST_FILESYSTEM_UNKNOWN, "ssh could not be run" },
        { PROC_RUN_TIMEOUT, 1, 0, "/bin/bash: Input/output error",
          GUEST_FILESYSTEM_UNKNOWN, "did not finish before its deadline" },
        { 130, 0, 1, "root 253:1 ext4 ro ro", GUEST_FILESYSTEM_UNKNOWN,
          "cancelled" },
        { 3, 0, 0, "--: no root mount in /proc/self/mountinfo",
          GUEST_FILESYSTEM_UNKNOWN, "no root mount in /proc/self/mountinfo (exit 3)" },
        { 0, 0, 0, "", GUEST_FILESYSTEM_UNKNOWN, "unexpected check output: (none)" },
        { 0, 0, 0, "root 253:1 ext4 rw", GUEST_FILESYSTEM_UNKNOWN,
          "unexpected check output: root 253:1 ext4 rw" },
        { 0, 0, 0, "root 253:1 ext4 rw rw extra\nerrors 0 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root  253:1 ext4 rw rw\nerrors 0 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, " root 253:1 ext4 rw rw\nerrors 0 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253 ext4 rw rw\nerrors 0 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253:x ext4 rw rw\nerrors 0 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253:1 ext4 rx rw\nerrors 0 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253:1 ext4 rw rwx\nerrors 0 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253:1 ext4 rw rw", GUEST_FILESYSTEM_UNKNOWN,
          "unexpected check output: (none)" },
        { 0, 0, 0, "root 253:1 ext4 rw rw\nerrors 0 0", GUEST_FILESYSTEM_UNKNOWN,
          "unexpected check output: errors 0 0" },
        { 0, 0, 0, "root 253:1 ext4 rw rw\nerrors 01 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253:1 ext4 rw rw\nerrors -1 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253:1 ext4 rw rw\nerrors 4294967296 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253:1 ext4 rw rw\nerrors 0 9223372036854775808 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected" },
        { 0, 0, 0, "root 253:1 ext4 rw rw\nerrors 0 0 0\nmore",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected check output: more" },
        /* A record that only ext4 keeps must not follow another type. */
        { 0, 0, 0, "root 0:30 btrfs rw rw\nerrors 0 0 0",
          GUEST_FILESYSTEM_UNKNOWN, "unexpected check output: errors 0 0 0" },
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        const struct classification *expected = &cases[index];
        struct guest_filesystem_check check;
        memset(&check, 'x', sizeof(check));
        guest_filesystem_classify(expected->rc, expected->output,
                                  expected->timed_out, expected->cancelled,
                                  &check);
        REQUIRE(check.state == expected->state &&
                strstr(check.detail, expected->detail),
                "case %zu: state %d, detail \"%s\"", index, check.state,
                check.detail);
        REQUIRE(guest_filesystem_damaged(&check) ==
                (expected->state == GUEST_FILESYSTEM_ERRORS_RECORDED ||
                 expected->state == GUEST_FILESYSTEM_UNREADABLE ||
                 expected->state == GUEST_FILESYSTEM_READ_ONLY),
                "case %zu: damaged", index);
    }
    /* A long guest line is cut inside the detail. */
    char line[1024];
    memset(line, 'a', sizeof(line));
    snprintf(line + sizeof(line) - 32, 32, ": Input/output error");
    line[0] = '/';
    struct guest_filesystem_check check;
    guest_filesystem_classify(1, line, 0, 0, &check);
    REQUIRE(check.state == GUEST_FILESYSTEM_UNREADABLE &&
            strlen(check.detail) == GUEST_FILESYSTEM_DETAIL_CAP - 1,
            "long line: %zu", strlen(check.detail));
    guest_filesystem_classify(0, NULL, 0, 0, &check);
    REQUIRE(check.state == GUEST_FILESYSTEM_UNKNOWN, "NULL output");
}

/* A stand-in for ssh. Mode guest runs the remote command, the last
 * argument, with bash as sshd runs it with the login shell, after pointing
 * the guest paths at the fixture; the other modes print what sshd or the
 * client print in a failure. */
static const char fake_ssh[] =
    "#!/bin/bash\n"
    "command=${!#}\n"
    "mode=${HAMN_TEST_SSH_MODE:?}\n"
    "if test \"$mode\" = home-eio; then\n"
    "  echo 'Could not chdir to home directory /home/hamn: Input/output "
    "error' >&2\n"
    "  mode=guest\n"
    "fi\n"
    "case $mode in\n"
    "guest)\n"
    "  command=${command//\\/proc\\/self\\/mountinfo/$HAMN_TEST_FIXTURE\\/mountinfo}\n"
    "  command=${command//\\/sys\\/dev\\/block/$HAMN_TEST_FIXTURE\\/devices}\n"
    "  command=${command//\\/sys\\/fs\\/ext4/$HAMN_TEST_FIXTURE\\/ext4}\n"
    "  exec /bin/bash -c \"$command\" ;;\n"
    "shell-eio) echo '/bin/bash: Input/output error' >&2; exit 1 ;;\n"
    "refused) echo 'ssh: connect to host 192.0.2.1 port 22: Connection "
    "refused' >&2; exit 255 ;;\n"
    /* Named so that a check for a survivor finds this run's alone. */
    "hang) exec -a \"$HAMN_TEST_FIXTURE/hang\" /bin/sleep 30 ;;\n"
    "esac\n"
    "exit 99\n";

static double seconds_since(const struct timespec *start)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (double)(now.tv_sec - start->tv_sec) +
           (double)(now.tv_nsec - start->tv_nsec) / 1e9;
}

static void probe_runs_through_ssh_without_writing(void)
{
    char bin[PATH_MAX], ssh[PATH_MAX], path_env[PATH_MAX + 32];
    snprintf(bin, sizeof(bin), "%s/bin", base);
    make_directory(bin);
    snprintf(ssh, sizeof(ssh), "%s/ssh", bin);
    write_file(ssh, fake_ssh, 0755);
    snprintf(path_env, sizeof(path_env), "%s:/usr/bin:/bin", bin);
    char *saved_path = strdup(getenv("PATH") ? getenv("PATH") : "");
    REQUIRE(setenv("PATH", path_env, 1) == 0, "PATH");

    struct fixture f;
    fixture_init(&f, "through-ssh");
    write_file(f.mountinfo, PROC_LINE ROOT_LINE, 0644);
    fixture_device(&f, "253:1", "vda1", "2", "1790000000", "1790000100");
    REQUIRE(setenv("HAMN_TEST_FIXTURE", f.root, 1) == 0, "fixture");
    struct profile profile;
    memset(&profile, 0, sizeof(profile));
    snprintf(profile.name, sizeof(profile.name), "fixture");
    snprintf(profile.dir, sizeof(profile.dir), "%s", base);

    const struct {
        const char *mode;
        enum guest_filesystem_state state;
        const char *detail;
    } cases[] = {
        { "guest", GUEST_FILESYSTEM_ERRORS_RECORDED, "ext4 recorded 2 errors" },
        { "home-eio", GUEST_FILESYSTEM_UNREADABLE,
          "Could not chdir to home directory /home/hamn: Input/output error" },
        { "shell-eio", GUEST_FILESYSTEM_UNREADABLE,
          "/bin/bash: Input/output error" },
        { "refused", GUEST_FILESYSTEM_UNKNOWN, "ssh failed: ssh: connect" },
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        REQUIRE(setenv("HAMN_TEST_SSH_MODE", cases[index].mode, 1) == 0, "mode");
        struct guest_filesystem_check check;
        guest_filesystem_probe(&profile, "192.0.2.1", 5000, &check);
        REQUIRE(check.state == cases[index].state &&
                strstr(check.detail, cases[index].detail),
                "%s: state %d, detail \"%s\"", cases[index].mode, check.state,
                check.detail);
    }

    /* A guest that never answers costs the deadline, not more. */
    REQUIRE(setenv("HAMN_TEST_SSH_MODE", "hang", 1) == 0, "mode");
    struct timespec started;
    clock_gettime(CLOCK_MONOTONIC, &started);
    struct guest_filesystem_check check;
    guest_filesystem_probe(&profile, "192.0.2.1", 300, &check);
    double elapsed = seconds_since(&started);
    REQUIRE(check.state == GUEST_FILESYSTEM_UNKNOWN &&
            strstr(check.detail, "deadline") && elapsed < 5.0,
            "hang: state %d after %.1f s (%s)", check.state, elapsed,
            check.detail);
    char output[64], marker[PATH_MAX];
    snprintf(marker, sizeof(marker), "%s/hang", f.root);
    const char *const leftover[] = { "/usr/bin/pgrep", "-f", marker, NULL };
    REQUIRE(run(leftover, NULL, output, sizeof(output)) == 1,
            "a stand-in ssh outlived its deadline: %s", output);

    /* The probe touched nothing in the fixture: it only reads. */
    char record[PATH_MAX];
    snprintf(record, sizeof(record), "%s/ext4/vda1/errors_count", f.root);
    char text[16];
    int fd = open(record, O_RDONLY);
    REQUIRE(fd >= 0 && read(fd, text, sizeof(text)) == 2 && close(fd) == 0 &&
            text[0] == '2', "the ext4 record changed");
    REQUIRE(setenv("PATH", saved_path, 1) == 0, "PATH");
    free(saved_path);
}

static cJSON *snapshot_of(const struct profile *profile)
{
    cJSON *value = guest_filesystem_snapshot(profile);
    REQUIRE(value, "snapshot ran out of memory");
    return value;
}

static void require_no_record(const struct profile *profile, const char *why)
{
    cJSON *value = snapshot_of(profile);
    REQUIRE(cJSON_IsNull(value), "%s: %s", why, cJSON_PrintUnformatted(value));
    cJSON_Delete(value);
}

static void require_unknown(const struct profile *profile, const char *why)
{
    cJSON *value = snapshot_of(profile);
    const cJSON *state = cJSON_GetObjectItemCaseSensitive(value, "state");
    const cJSON *detail = cJSON_GetObjectItemCaseSensitive(value, "detail");
    REQUIRE(cJSON_IsString(state) && !strcmp(state->valuestring, "unknown") &&
            cJSON_IsString(detail) && detail->valuestring[0] &&
            cJSON_IsNull(cJSON_GetObjectItemCaseSensitive(value, "observedAt")) &&
            cJSON_GetArraySize(value) == 3,
            "%s: %s", why, cJSON_PrintUnformatted(value));
    cJSON_Delete(value);
}

/* The state, detail and observedAt of the reported record. */
static void require_record(const struct profile *profile, const char *state,
                           const char *detail, double earliest)
{
    cJSON *value = snapshot_of(profile);
    const cJSON *item = cJSON_GetObjectItemCaseSensitive(value, "state");
    const cJSON *text = cJSON_GetObjectItemCaseSensitive(value, "detail");
    const cJSON *observed = cJSON_GetObjectItemCaseSensitive(value, "observedAt");
    REQUIRE(cJSON_IsString(item) && !strcmp(item->valuestring, state) &&
            cJSON_IsString(text) && !strcmp(text->valuestring, detail) &&
            cJSON_IsNumber(observed) && observed->valuedouble >= earliest &&
            observed->valuedouble <= (double)time(NULL) &&
            cJSON_GetArraySize(value) == 3,
            "record: %s", cJSON_PrintUnformatted(value));
    cJSON_Delete(value);
}

static void replace_disk(const char *disk)
{
    char staged[PATH_MAX];
    snprintf(staged, sizeof(staged), "%s.new", disk);
    write_file(staged, "another disk\n", 0600);
    REQUIRE(rename(staged, disk) == 0, "rename disk");
}

static void the_record_describes_one_disk_and_is_validated(void)
{
    char home[PATH_MAX], hamn[PATH_MAX];
    snprintf(home, sizeof(home), "%s/home", base);
    make_directory(home);
    REQUIRE(setenv("HOME", home, 1) == 0, "HOME");
    REQUIRE(hamn_control_configure("fixture", 0, 0, 0, 1, -1) == 0,
            "create the profile");
    struct profile profile;
    REQUIRE(profile_read_existing(&profile, "fixture") == 0, "read profile");
    snprintf(hamn, sizeof(hamn), "%s/.hamn/fixture", home);
    REQUIRE(strcmp(profile.dir, hamn) == 0, "profile directory %s", profile.dir);
    char disk[PATH_MAX], record[PATH_MAX];
    profile_path(&profile, "disk.img", disk, sizeof(disk));
    profile_path(&profile, "guest-filesystem.json", record, sizeof(record));

    /* No disk and no record: nothing to report, and nothing to record. */
    require_no_record(&profile, "no disk");
    struct guest_filesystem_check check;
    guest_filesystem_classify(0, "root 253:1 ext4 rw rw\nerrors 2 1790000000 "
                              "1790000100", 0, 0, &check);
    errno = 0;
    REQUIRE(guest_filesystem_record(&profile, &check) == -1 && errno == ENOENT,
            "a record without a disk");
    REQUIRE(access(record, F_OK) != 0, "a record was written without a disk");

    write_file(disk, "disk\n", 0600);
    require_no_record(&profile, "no record");
    double before = (double)time(NULL);
    REQUIRE(guest_filesystem_record(&profile, &check) == 0, "record: %s",
            strerror(errno));
    require_record(&profile, "errorsRecorded", check.detail, before);
    struct stat status;
    REQUIRE(lstat(record, &status) == 0 && S_ISREG(status.st_mode) &&
            (status.st_mode & 0777) == 0600, "record mode");

    /* `vm status` reports it, whether or not the VM runs. */
    char *text = NULL;
    REQUIRE(hamn_control_query("fixture", &text) == 0, "query");
    cJSON *query = cJSON_Parse(text);
    hamn_control_free(text);
    const cJSON *reported = cJSON_GetObjectItemCaseSensitive(query, "guestFilesystem");
    REQUIRE(cJSON_IsObject(reported) &&
            !strcmp(cJSON_GetObjectItemCaseSensitive(reported, "state")->valuestring,
                    "errorsRecorded"), "vm status: %s",
            cJSON_PrintUnformatted(query));
    cJSON_Delete(query);

    /* Only a damaged check is recorded; the earlier record stays. */
    struct guest_filesystem_check healthy;
    guest_filesystem_classify(0, HEALTHY_OUTPUT, 0, 0, &healthy);
    errno = 0;
    REQUIRE(guest_filesystem_record(&profile, &healthy) == -1 && errno == EINVAL,
            "a healthy check was recorded");
    require_record(&profile, "errorsRecorded", check.detail, before);

    /* A failed write keeps the earlier record. */
    struct guest_filesystem_check unreadable;
    guest_filesystem_classify(1, "/bin/bash: Input/output error", 0, 0,
                              &unreadable);
    REQUIRE(chmod(profile.dir, 0500) == 0, "chmod");
    int recorded = guest_filesystem_record(&profile, &unreadable);
    int error = errno;
    REQUIRE(chmod(profile.dir, 0700) == 0, "chmod");
    REQUIRE(recorded == -1 && error == EACCES, "write into a read-only "
            "directory: %d %s", recorded, strerror(error));
    require_record(&profile, "errorsRecorded", check.detail, before);
    REQUIRE(guest_filesystem_record(&profile, &unreadable) == 0, "rerecord");
    require_record(&profile, "unreadable", unreadable.detail, before);

    /* A disk that replaced the observed one has not been checked; the
     * record applies again to nothing, even after the new disk is also
     * damaged and recorded. */
    replace_disk(disk);
    require_no_record(&profile, "replaced disk");
    REQUIRE(guest_filesystem_record(&profile, &check) == 0, "record new disk");
    require_record(&profile, "errorsRecorded", check.detail, before);
    REQUIRE(unlink(disk) == 0, "unlink disk");
    require_no_record(&profile, "removed disk");
    write_file(disk, "disk\n", 0600);
    require_no_record(&profile, "recreated disk");
    REQUIRE(guest_filesystem_record(&profile, &check) == 0, "record");

    /* Records that the writer cannot have written are reported as such. */
    char valid[1024];
    int fd = open(record, O_RDONLY);
    ssize_t length = read(fd, valid, sizeof(valid) - 1);
    REQUIRE(fd >= 0 && length > 0 && close(fd) == 0, "read record");
    valid[length] = '\0';
    char identity[64];
    snprintf(identity, sizeof(identity), "%s",
             strstr(valid, "\"disk\":\"") + 8);
    identity[strcspn(identity, "\"")] = '\0';
    const char *invalid[] = {
        "{",
        "[]",
        "{\"schemaVersion\":2,\"state\":\"unreadable\",\"detail\":\"x\","
        "\"observedAt\":1,\"disk\":\"%s\"}",
        "{\"schemaVersion\":1,\"state\":\"healthy\",\"detail\":\"x\","
        "\"observedAt\":1,\"disk\":\"%s\"}",
        "{\"schemaVersion\":1,\"state\":\"unreadable\",\"detail\":1,"
        "\"observedAt\":1,\"disk\":\"%s\"}",
        "{\"schemaVersion\":1,\"state\":\"unreadable\",\"detail\":\"x\","
        "\"observedAt\":-1,\"disk\":\"%s\"}",
        "{\"schemaVersion\":1,\"state\":\"unreadable\",\"detail\":\"x\","
        "\"observedAt\":1.5,\"disk\":\"%s\"}",
        "{\"schemaVersion\":1,\"state\":\"unreadable\",\"detail\":\"x\","
        "\"observedAt\":1,\"disk\":\"%s\",\"extra\":1}",
        "{\"schemaVersion\":1,\"state\":\"unreadable\",\"state\":\"x\","
        "\"observedAt\":1,\"disk\":\"%s\"}",
        "{\"schemaVersion\":1,\"state\":\"unreadable\",\"detail\":\"x\","
        "\"observedAt\":1,\"disk\":\"%s\"} trailing",
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        char text_record[1024];
        snprintf(text_record, sizeof(text_record), invalid[index], identity);
        write_file(record, text_record, 0600);
        require_unknown(&profile, text_record);
    }
    char long_detail[600];
    memset(long_detail, 'd', 300);
    long_detail[300] = '\0';
    char text_record[1024];
    snprintf(text_record, sizeof(text_record),
             "{\"schemaVersion\":1,\"state\":\"unreadable\",\"detail\":\"%s\","
             "\"observedAt\":1,\"disk\":\"%s\"}", long_detail, identity);
    write_file(record, text_record, 0600);
    require_unknown(&profile, "a detail longer than the writer's");
    write_file(record, valid, 0644);
    require_unknown(&profile, "a record readable by others");
    write_file(record, "", 0600);
    require_unknown(&profile, "an empty record");
    write_file(record, valid, 0600);
    char second_name[PATH_MAX];
    snprintf(second_name, sizeof(second_name), "%s.link", record);
    REQUIRE(link(record, second_name) == 0, "hard link");
    require_unknown(&profile, "a record with two names");
    REQUIRE(unlink(second_name) == 0, "unlink");
    REQUIRE(unlink(record) == 0 && symlink("disk.img", record) == 0, "symlink");
    require_unknown(&profile, "a symlinked record");
    REQUIRE(unlink(record) == 0, "unlink");

    /* forget removes the record, and succeeds when there is none. */
    write_file(record, valid, 0600);
    require_record(&profile, "errorsRecorded", check.detail, 0);
    REQUIRE(guest_filesystem_forget(&profile) == 0, "forget");
    require_no_record(&profile, "forgotten");
    REQUIRE(guest_filesystem_forget(&profile) == 0, "forget twice");
    REQUIRE(access(record, F_OK) != 0, "forgotten record exists");
}

int main(void)
{
    char temporary[] = "/tmp/hamn-guest-fs-XXXXXX";
    REQUIRE(mkdtemp(temporary), "mkdtemp");
    REQUIRE(realpath(temporary, base), "realpath");
    pid_t runner = fork();
    REQUIRE(runner >= 0, "fork");
    if (runner == 0) {
        if (setpgid(0, 0) != 0)
            _exit(94);
        alarm(60);
        choose_interpreters();
        probe_script_reports_the_root_filesystem_and_its_ext4_record();
        classification_needs_exact_evidence();
        probe_runs_through_ssh_without_writing();
        the_record_describes_one_disk_and_is_validated();
        _exit(0);
    }
    int status = 0;
    while (waitpid(runner, &status, 0) < 0 && errno == EINTR) {}
    if (kill(-runner, SIGKILL) != 0 && errno != ESRCH)
        fail("cannot end what the cases left running");
    char command[PATH_MAX + 32];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", base);
    int removed = system(command) == 0;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: guest filesystem cases (wait status %d)\n",
                status);
        return 1;
    }
    REQUIRE(removed, "cannot remove %s", base);
    puts("PASS: the guest filesystem check reads exact evidence, and its "
         "record names one disk");
    return 0;
}
