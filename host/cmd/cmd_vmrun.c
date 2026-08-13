/*
 * hamn vmrun — VM을 소유하는 포그라운드 프로세스.
 *
 * Virtualization.framework의 VZVirtualMachine은 살아있는 프로세스가 소유해야
 * 하므로, hamn start(M1)는 이 커맨드를 데몬으로 spawn한다. M0에서는 부팅
 * 스파이크(S2) 검증을 위해 직접 실행한다.
 *
 * SIGINT/SIGTERM: 게스트에 graceful stop 요청 후 15초 내 미종료 시 강제 종료.
 */

#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <limits.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>

#include <dispatch/dispatch.h>

#include "cli.h"
#include "core/log.h"
#include "util/fs.h"
#include "util/proc.h"
#include "vmrun/ctlsock.h"
#include "vz/vz_shim.h"

#define STOP_GRACE_SEC 15

static vz_vm *g_vm;
static const char *g_pidfile;
static const char *g_identity_file;
static const char *g_ctl_sock;
static char g_pid_text[32];
static char g_identity_text[192];
static size_t g_pid_text_len;
static size_t g_identity_text_len;
static int g_pid_owned;
static int g_identity_owned;
static dev_t g_ctl_dev;
static ino_t g_ctl_ino;
static int g_ctl_owned;
static const char *g_spawn_guard_path;
static const char *g_spawn_token;
static int g_spawn_guard_fd = -1;
static dev_t g_spawn_guard_dev;
static ino_t g_spawn_guard_ino;
static int g_spawn_guard_owned;

static int path_is_inode(const char *path, dev_t dev, ino_t ino,
                         mode_t type)
{
    struct stat sb;
    return path && lstat(path, &sb) == 0 && (sb.st_mode & S_IFMT) == type &&
           sb.st_dev == dev && sb.st_ino == ino;
}

static int regular_file_matches(const char *path, const char *expected,
                                size_t expected_len)
{
    if (!path || !expected || expected_len >= 256)
        return 0;
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0)
        return 0;
    struct stat open_sb, path_sb;
    char buf[256];
    ssize_t n = pread(fd, buf, expected_len + 1, 0);
    int matches = fstat(fd, &open_sb) == 0 && S_ISREG(open_sb.st_mode) &&
                  lstat(path, &path_sb) == 0 && S_ISREG(path_sb.st_mode) &&
                  open_sb.st_dev == path_sb.st_dev &&
                  open_sb.st_ino == path_sb.st_ino &&
                  n == (ssize_t)expected_len &&
                  memcmp(buf, expected, expected_len) == 0;
    close(fd);
    return matches;
}

static int sync_parent_dir(const char *path)
{
    char dir[PATH_MAX];
    size_t len = strlen(path);
    if (len == 0 || len >= sizeof(dir))
        return -1;
    memcpy(dir, path, len + 1);
    char *slash = strrchr(dir, '/');
    if (!slash)
        snprintf(dir, sizeof(dir), ".");
    else if (slash == dir)
        slash[1] = '\0';
    else
        *slash = '\0';
    int fd = open(dir, O_RDONLY | O_DIRECTORY);
    if (fd < 0)
        return -1;
    int rc = fsync(fd);
    close(fd);
    return rc;
}

static void cleanup_files(void)
{
    if (g_identity_owned &&
        regular_file_matches(g_identity_file, g_identity_text,
                             g_identity_text_len)) {
        if (g_pid_owned &&
            regular_file_matches(g_pidfile, g_pid_text, g_pid_text_len))
            (void)unlink(g_pidfile);
        (void)unlink(g_identity_file);
    }
    if (g_ctl_owned)
        (void)ctlsock_unlink_owned(g_ctl_sock, g_ctl_dev, g_ctl_ino);
    if (g_spawn_guard_owned && g_spawn_guard_fd >= 0 &&
        path_is_inode(g_spawn_guard_path, g_spawn_guard_dev,
                      g_spawn_guard_ino, S_IFREG))
        (void)unlink(g_spawn_guard_path);
    if (g_spawn_guard_fd >= 0) {
        close(g_spawn_guard_fd);
        g_spawn_guard_fd = -1;
    }
    const char *test_ready = getenv("HAMN_TEST_VMRUN_CLEANUP_READY_FIFO");
    if (test_ready && test_ready[0]) {
        FILE *ready = fopen(test_ready, "w");
        if (ready) {
            (void)fprintf(ready, "cleaned\n");
            (void)fclose(ready);
        }
    }
}

static int identity_files_write(uint64_t sec, uint64_t usec,
                                const unsigned char executable_uuid[16])
{
    char executable_uuid_hex[33];
    proc_executable_uuid_format(executable_uuid, executable_uuid_hex);
    int identity_len = snprintf(g_identity_text, sizeof(g_identity_text),
                                "%d %llu %llu %s\n", getpid(),
                                (unsigned long long)sec,
                                (unsigned long long)usec,
                                executable_uuid_hex);
    int pid_len = snprintf(g_pid_text, sizeof(g_pid_text), "%d\n", getpid());
    if (identity_len <= 0 || identity_len >= (int)sizeof(g_identity_text) ||
        pid_len <= 0 || pid_len >= (int)sizeof(g_pid_text))
        return -1;
    if (fs_write_file_atomic(g_identity_file, g_identity_text,
                             (size_t)identity_len, 0600) != 0)
        return -1;
    g_identity_text_len = (size_t)identity_len;
    g_identity_owned = 1;
    if (fs_write_file_atomic(g_pidfile, g_pid_text,
                             (size_t)pid_len, 0600) != 0)
        return -1;
    g_pid_text_len = (size_t)pid_len;
    g_pid_owned = 1;
    return 0;
}

static int spawn_guard_validate(void)
{
    if (g_spawn_guard_fd < 3 || !g_spawn_guard_path ||
        !g_spawn_guard_path[0] || !g_spawn_token ||
        strlen(g_spawn_token) != 32)
        return -1;
    for (int i = 0; i < 32; i++) {
        char ch = g_spawn_token[i];
        if (!((ch >= '0' && ch <= '9') || (ch >= 'a' && ch <= 'f')))
            return -1;
    }

    struct stat fd_sb, path_sb;
    if (fstat(g_spawn_guard_fd, &fd_sb) != 0 ||
        !S_ISREG(fd_sb.st_mode) || fd_sb.st_uid != geteuid() ||
        (fd_sb.st_mode & 077) != 0 || fd_sb.st_nlink != 1 ||
        lstat(g_spawn_guard_path, &path_sb) != 0 ||
        !S_ISREG(path_sb.st_mode) || fd_sb.st_dev != path_sb.st_dev ||
        fd_sb.st_ino != path_sb.st_ino)
        return -1;
    char expected[34], actual[35];
    int expected_len = snprintf(expected, sizeof(expected), "%s\n",
                                g_spawn_token);
    ssize_t n = pread(g_spawn_guard_fd, actual, sizeof(actual), 0);
    if (expected_len != 33 || n != expected_len ||
        memcmp(actual, expected, (size_t)expected_len) != 0 ||
        flock(g_spawn_guard_fd, LOCK_EX | LOCK_NB) != 0)
        return -1;
    g_spawn_guard_dev = fd_sb.st_dev;
    g_spawn_guard_ino = fd_sb.st_ino;
    g_spawn_guard_owned = 1;
    return 0;
}

static int spawn_guard_complete(void)
{
    if (!g_spawn_guard_owned || g_spawn_guard_fd < 0)
        return 0;
    if (!path_is_inode(g_spawn_guard_path, g_spawn_guard_dev,
                       g_spawn_guard_ino, S_IFREG) ||
        unlink(g_spawn_guard_path) != 0 ||
        sync_parent_dir(g_spawn_guard_path) != 0)
        return -1;
    g_spawn_guard_owned = 0;
    int fd = g_spawn_guard_fd;
    g_spawn_guard_fd = -1;
    return close(fd);
}

static int test_barrier(const char *ready_env, const char *release_env)
{
    const char *ready_path = getenv(ready_env);
    const char *release_path = getenv(release_env);
    if ((!ready_path || !ready_path[0]) &&
        (!release_path || !release_path[0]))
        return 0;
    if (!ready_path || !ready_path[0] || !release_path || !release_path[0])
        return -1;

    FILE *ready = fopen(ready_path, "w");
    if (!ready)
        return -1;
    int failed = fprintf(ready, "%d\n", getpid()) < 0;
    if (fclose(ready) != 0)
        failed = 1;
    if (failed)
        return -1;

    FILE *release = fopen(release_path, "r");
    if (!release)
        return -1;
    char line[32];
    int ok = fgets(line, sizeof(line), release) != NULL &&
             strcmp(line, "release\n") == 0;
    fclose(release);
    return ok ? 0 : -1;
}

static void on_state(void *ud, enum vz_state st)
{
    (void)ud;
    switch (st) {
    case VZ_ST_STOPPED:
        logmsg("vmrun: guest stopped");
        exit(0);
    case VZ_ST_ERROR:
        logerr("vmrun: vm entered error state");
        exit(1);
    default:
        break;
    }
}

static void handle_stop_signal(void)
{
    static int requested;
    char *err = NULL;

    if (requested) {
        logmsg("vmrun: force stopping");
        vz_vm_force_stop(g_vm, &err);
        exit(0);
    }
    requested = 1;

    logmsg("vmrun: requesting guest stop");
    if (vz_vm_request_stop(g_vm, &err) != 0) {
        logerr("vmrun: %s; force stopping", err ? err : "request failed");
        free(err);
        err = NULL;
        vz_vm_force_stop(g_vm, &err);
        exit(0);
    }
    /* 유예 시간 내에 게스트가 멈추지 않으면 강제 종료 */
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,
                                 STOP_GRACE_SEC * NSEC_PER_SEC),
                   dispatch_get_main_queue(), ^{
                       logmsg("vmrun: stop grace period expired, forcing");
                       char *e = NULL;
                       vz_vm_force_stop(g_vm, &e);
                       exit(0);
                   });
}

static void watch_signal(int sig)
{
    signal(sig, SIG_IGN);
    dispatch_source_t src = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_SIGNAL, (uintptr_t)sig, 0,
        dispatch_get_main_queue());
    dispatch_source_set_event_handler(src, ^{
        handle_stop_signal();
    });
    dispatch_resume(src);
    /* dispatch source는 프로세스 수명 동안 유지 (의도적 누수) */
}

static int vmrun_usage(void)
{
    fprintf(stderr,
            "usage: hamn vmrun --disk <raw> [options]\n"
            "  --disk <path>         raw disk image (required, rw)\n"
            "  --seed <path>         cloud-init seed ISO (ro)\n"
            "  --serial-log <path>   serial console log file\n"
            "  --efi-vars <path>     EFI variable store (default: <disk>.efivars)\n"
            "  --machine-id <path>   persisted machine identifier\n"
            "  --mac <addr>          fixed MAC address\n"
            "  --cpus <n>            vCPU count (default 4)\n"
            "  --mem-mib <n>         memory in MiB (default 4096)\n"
            "  --rosetta BOOL        expose Linux Intel translation (opt-in)\n"
            "  --nested-virtualization BOOL enable supported nested VZ\n"
            "  --share <tag>=<path>  writable virtiofs share (repeatable)\n"
            "  --share-ro <tag>=<path> read-only virtiofs share (repeatable, max %d)\n",
            VZ_MAX_SHARES);
    return 1;
}

static int parse_bool_option(const char *text, int *output)
{
    if (strcmp(text, "true") == 0) {
        *output = 1;
        return 0;
    }
    if (strcmp(text, "false") == 0) {
        *output = 0;
        return 0;
    }
    return -1;
}

int cmd_vmrun(int argc, char **argv)
{
    vz_vm_spec spec = {
        .cpus = 4,
        .mem_bytes = 4096ULL << 20,
    };
    char efi_default[1024];

    static const struct option opts[] = {
        { "disk", required_argument, NULL, 'd' },
        { "seed", required_argument, NULL, 's' },
        { "serial-log", required_argument, NULL, 'l' },
        { "efi-vars", required_argument, NULL, 'e' },
        { "machine-id", required_argument, NULL, 'i' },
        { "mac", required_argument, NULL, 'm' },
        { "cpus", required_argument, NULL, 'c' },
        { "mem-mib", required_argument, NULL, 'M' },
        { "share", required_argument, NULL, 'S' },
        { "share-ro", required_argument, NULL, 'R' },
        { "rosetta", required_argument, NULL, 'r' },
        { "nested-virtualization", required_argument, NULL, 'v' },
        { "pidfile", required_argument, NULL, 'p' },
        { "identity-file", required_argument, NULL, 'I' },
        { "spawn-guard", required_argument, NULL, 'G' },
        { "spawn-guard-fd", required_argument, NULL, 'F' },
        { "spawn-token", required_argument, NULL, 'T' },
        { "ctl-sock", required_argument, NULL, 'k' },
        { 0 },
    };

    optind = 1;
    optreset = 1;
    int ch;
    while ((ch = getopt_long(argc, argv, "", opts, NULL)) != -1) {
        switch (ch) {
        case 'd':
            spec.disk_img = optarg;
            break;
        case 's':
            spec.seed_iso = optarg;
            break;
        case 'l':
            spec.serial_log = optarg;
            break;
        case 'e':
            spec.efi_vars = optarg;
            break;
        case 'i':
            spec.machine_id_file = optarg;
            break;
        case 'm':
            spec.mac_addr = optarg;
            break;
        case 'c':
            spec.cpus = (unsigned)atoi(optarg);
            break;
        case 'M':
            spec.mem_bytes = (uint64_t)atoll(optarg) << 20;
            break;
        case 'p':
            g_pidfile = optarg;
            break;
        case 'I':
            g_identity_file = optarg;
            break;
        case 'G':
            g_spawn_guard_path = optarg;
            break;
        case 'F': {
            char *end = NULL;
            long fd = strtol(optarg, &end, 10);
            if (!end || *end != '\0' || fd < 3 || fd > INT_MAX)
                die("vmrun: invalid --spawn-guard-fd");
            g_spawn_guard_fd = (int)fd;
            break;
        }
        case 'T':
            g_spawn_token = optarg;
            break;
        case 'k':
            g_ctl_sock = optarg;
            break;
        case 'S':
        case 'R': {
            if (spec.nshares >= VZ_MAX_SHARES)
                die("vmrun: too many shares (max %d)", VZ_MAX_SHARES);
            char *eq = strchr(optarg, '=');
            if (!eq || eq == optarg || !eq[1])
                die("vmrun: --share must be <tag>=<path>");
            *eq = '\0';
            spec.shares[spec.nshares].tag = optarg;
            spec.shares[spec.nshares].host_path = eq + 1;
            spec.shares[spec.nshares].read_only = ch == 'R';
            spec.nshares++;
            break;
        }
        case 'r':
            if (parse_bool_option(optarg, &spec.rosetta) != 0)
                die("vmrun: --rosetta must be true or false");
            break;
        case 'v':
            if (parse_bool_option(optarg, &spec.nested_virtualization) != 0)
                die("vmrun: --nested-virtualization must be true or false");
            break;
        default:
            return vmrun_usage();
        }
    }
    if (!spec.disk_img)
        return vmrun_usage();
    if (!!g_pidfile != !!g_identity_file) {
        logerr("vmrun: --pidfile and --identity-file must be used together");
        return 1;
    }
    int guard_args = !!g_spawn_guard_path + (g_spawn_guard_fd >= 0) +
                     !!g_spawn_token;
    if (guard_args != 0 && guard_args != 3) {
        logerr("vmrun: spawn guard path, fd, and token must be used together");
        return 1;
    }
    if (guard_args && !g_pidfile) {
        logerr("vmrun: spawn guard requires process identity files");
        return 1;
    }
    if (!spec.efi_vars) {
        snprintf(efi_default, sizeof(efi_default), "%s.efivars",
                 spec.disk_img);
        spec.efi_vars = efi_default;
    }

    atexit(cleanup_files);
    if (guard_args && spawn_guard_validate() != 0)
        die("vmrun: cannot validate inherited spawn guard");
    const char *test_exit =
        getenv("HAMN_TEST_VMRUN_EXIT_BEFORE_IDENTITY");
    if (test_exit && strcmp(test_exit, "1") == 0)
        _exit(86);
    if (test_barrier("HAMN_TEST_VMRUN_PRE_IDENTITY_READY_FIFO",
                     "HAMN_TEST_VMRUN_PRE_IDENTITY_RELEASE_FIFO") != 0)
        die("vmrun: pre-identity startup test barrier failed");
    uint64_t start_sec = 0, start_usec = 0;
    unsigned char executable_uuid[16];
    if (g_pidfile) {
        if (proc_start_identity(getpid(), &start_sec, &start_usec) != 0 ||
            proc_executable_identity(getpid(), executable_uuid) != 0 ||
            identity_files_write(start_sec, start_usec,
                                 executable_uuid) != 0)
            die("vmrun: cannot persist process identity");
    }
    if (spawn_guard_complete() != 0)
        die("vmrun: cannot complete process identity handoff");
    if (test_barrier("HAMN_TEST_VMRUN_PRE_CTL_READY_FIFO",
                     "HAMN_TEST_VMRUN_PRE_CTL_RELEASE_FIFO") != 0)
        die("vmrun: pre-control startup test barrier failed");

    char *err = NULL;
    g_vm = vz_vm_create(&spec, on_state, NULL, &err);
    if (!g_vm)
        die("vmrun: %s", err ? err : "vm creation failed");

    if (vz_vm_start(g_vm, &err) != 0)
        die("vmrun: %s", err ? err : "vm start failed");

    logmsg("vmrun: vm started (cpus=%u mem=%lluMiB disk=%s)", spec.cpus,
           (unsigned long long)(spec.mem_bytes >> 20), spec.disk_img);

    if (g_ctl_sock) {
        if (ctlsock_serve(g_ctl_sock, g_vm, start_sec, start_usec,
                          handle_stop_signal, &g_ctl_dev,
                          &g_ctl_ino) != 0)
            die("vmrun: cannot serve control socket %s", g_ctl_sock);
        g_ctl_owned = 1;
    }

    watch_signal(SIGINT);
    watch_signal(SIGTERM);
    vz_runloop();
}
