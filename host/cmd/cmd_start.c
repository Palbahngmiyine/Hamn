/*
 * hamn start — 부팅 오케스트레이션 (멱등).
 *
 * 이미지 캐시 → 디스크 준비 → ssh 키 → seed ISO → vmrun spawn →
 * DHCP IP 발견 → ssh master 기동 순서로 진행하며, 각 단계는 이미
 * 완료되어 있으면 건너뛴다.
 */

#include <errno.h>
#include <getopt.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "cli.h"
#include "core/guest_deployment.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/mutation_lock.h"
#include "core/profile.h"
#include "core/provision.h"
#include "core/state.h"
#include "fwd/docker_observer.h"
#include "fwd/mount_inotify.h"
#include "image/disk.h"
#include "image/fetch.h"
#include "seed/cloudinit.h"
#include "sshmgr/ssh.h"
#include "util/dhcp_leases.h"
#include "util/fs.h"
#include "util/proc.h"
#include "vmrun/ctlsock.h"
#include "vz/vz_shim.h"

#define START_REEXEC_AFTER_SIGNED_UPDATE 3

static int mac_ensure(const struct profile *p, char *mac, size_t cap)
{
    char path[1024];
    profile_path(p, "mac-addr", path, sizeof(path));

    FILE *f = fopen(path, "r");
    if (f) {
        int ok = fgets(mac, (int)cap, f) != NULL;
        fclose(f);
        if (ok) {
            mac[strcspn(mac, "\r\n")] = '\0';
            if (strlen(mac) == 17)
                return 0;
        }
    }
    /* locally administered, QEMU/KVM 관례 prefix 52:54:00 */
    snprintf(mac, cap, "52:54:00:%02x:%02x:%02x",
             (unsigned)arc4random_uniform(256),
             (unsigned)arc4random_uniform(256),
             (unsigned)arc4random_uniform(256));
    f = fopen(path, "w");
    if (!f)
        return -1;
    fprintf(f, "%s\n", mac);
    fclose(f);
    return 0;
}

static int path_is_within(const char *base, const char *path)
{
    size_t length = strlen(base);
    return strncmp(base, path, length) == 0 &&
        (base[length - 1] == '/' || path[length] == '\0' || path[length] == '/');
}

static int canonical_owned_directory(const char *path, char output[PATH_MAX],
                                     int reject_symlinks)
{
    struct stat status;
    if (!path || !path[0] || lstat(path, &status) != 0 ||
        !S_ISDIR(status.st_mode) || status.st_uid != geteuid() ||
        !realpath(path, output))
        return -1;
    if (reject_symlinks && strcmp(path, output) != 0) {
        errno = ELOOP;
        return -1;
    }
    return 0;
}

static int append_vm_share(char entries[VZ_MAX_SHARES][PATH_MAX + 32],
                           int read_only[VZ_MAX_SHARES], size_t *count,
                           const char *tag, const char *host_path,
                           int is_read_only)
{
    if (*count == VZ_MAX_SHARES)
        return -1;
    int written = snprintf(entries[*count], PATH_MAX + 32, "%s=%s", tag,
                           host_path);
    if (written < 0 || written >= PATH_MAX + 32)
        return -1;
    read_only[*count] = is_read_only;
    (*count)++;
    return 0;
}

static int append_vm_argument(const char **arguments, size_t *count,
                              size_t capacity, const char *argument)
{
    if (!argument || *count + 1 >= capacity)
        return -1;
    arguments[(*count)++] = argument;
    return 0;
}

/* PID만 살아 있는 중간 실패 상태를 정상 start로 오인하지 않는다. */
static int running_start_ready(const struct profile *p)
{
    struct vm_state st;
    if (state_load(p, &st) != 0 || strcmp(st.state, "running") != 0 ||
        !st.ip[0] || ssh_master_alive(p) != 0)
        return 0;

    return guest_deployment_runtime_ready(p, st.ip, 1) == 0;
}

static int rollback_incomplete_start(const struct profile *p)
{
    logmsg("rolling back incomplete start ...");
    if (vm_stop(p, NULL) != 0) {
        logerr("cannot completely roll back failed start");
        return -1;
    }
    return 0;
}

/* 정식 Docker CLI가 있으면 이 프로필만 소유한 context를 전환한다. */
static int setup_docker_context(const struct profile *p,
                                struct vm_state *st, const char *sock)
{
    char previous[128] = "";
    const char *show[] = { "docker", "context", "show", NULL };
    if (proc_run_capture(show, previous, sizeof(previous)) != 0) {
        logmsg("note: Docker CLI was not found; install Docker CLI separately "
               "or use 'hamn env' with an SDK");
        return 0;
    }
    char context[128], endpoint[1100], hostarg[1100];
    if (profile_docker_context_name(p, context, sizeof(context)) != 0) {
        logerr("cannot resolve Docker context for profile %s", p->name);
        return -1;
    }
    snprintf(endpoint, sizeof(endpoint), "unix://%s", sock);
    snprintf(hostarg, sizeof(hostarg), "host=%s", endpoint);

    char output[512] = "";
    const char *inspect[] = {
        "docker", "context", "inspect", context, "--format",
        "{{.Endpoints.docker.Host}}", NULL
    };
    if (proc_run_capture(inspect, output, sizeof(output)) != 0) {
        output[0] = '\0';
        const char *create[] = { "docker", "context", "create", context,
                                 "--docker", hostarg, NULL };
        if (proc_run_capture(create, output, sizeof(output)) != 0) {
            logerr("docker context '%s' create failed: %s", context,
                   output[0] ? output : "no output");
            return -1;
        }
    } else if (strcmp(output, endpoint) != 0) {
        logerr("Docker context '%s' belongs to another endpoint (%s); "
               "Hamn will not overwrite it", context, output);
        return -1;
    }

    if (strcmp(previous, context) != 0) {
        snprintf(st->prev_docker_context, sizeof(st->prev_docker_context),
                 "%s", previous);
        if (state_save(p, st) != 0) {
            logerr("cannot persist previous Docker context");
            st->prev_docker_context[0] = '\0';
            return -1;
        }
    }
    output[0] = '\0';
    const char *use[] = { "docker", "context", "use", context, NULL };
    if (proc_run_capture(use, output, sizeof(output)) != 0) {
        logerr("docker context '%s' use failed: %s", context,
               output[0] ? output : "no output");
        if (strcmp(previous, context) != 0) {
            st->prev_docker_context[0] = '\0';
            if (state_save(p, st) != 0)
                logerr("cannot clear the failed Docker context activation");
        }
        return -1;
    }
    logmsg("docker context '%s' is now active", context);
    return 0;
}

/* 실행 중인 VM도 Docker CLI/context가 뒤늦게 바뀔 수 있으므로, profile
 * socket을 기준으로 context 소유권과 활성화를 다시 확인한다. */
static int retry_running_docker_context(const struct profile *p,
                                        struct vm_state *st)
{
    char socket_path[1024];
    if (!profile_path(p, "docker.sock", socket_path, sizeof(socket_path))) {
        logerr("cannot resolve profile Docker socket");
        return -1;
    }
    return setup_docker_context(p, st, socket_path);
}

#ifdef HAMN_TEST
/* Test-only seam for the already-running start branch. */
int hamn_test_start_retry_running_docker_context(const struct profile *p,
                                                 struct vm_state *st)
{
    return retry_running_docker_context(p, st);
}
#endif

static int wait_vmrun_running(const struct profile *p, const char *ctl,
                              int timeout_sec)
{
    char resp[256];
    for (int i = 0; i < timeout_sec * 2; i++) {
        if (ctlsock_query(ctl, "{\"cmd\":\"status\"}", resp, sizeof(resp),
                          300) == 0 &&
            strstr(resp, "\"running\"") && vm_running_pid(p) > 0)
            return 0;
        usleep(500 * 1000);
    }
    return -1;
}

static int test_identity_timeout_dsec(void)
{
    const char *value = getenv("HAMN_TEST_VMRUN_IDENTITY_TIMEOUT_DSEC");
    if (!value || !value[0])
        return 50;
    char *end = NULL;
    long timeout = strtol(value, &end, 10);
    if (!end || *end != '\0' || timeout < 1 || timeout > 50)
        return -1;
    return (int)timeout;
}

static int test_pre_capture_barrier(pid_t pid)
{
    const char *ready_path =
        getenv("HAMN_TEST_START_PRE_CAPTURE_READY_FIFO");
    const char *release_path =
        getenv("HAMN_TEST_START_PRE_CAPTURE_RELEASE_FIFO");
    if ((!ready_path || !ready_path[0]) &&
        (!release_path || !release_path[0]))
        return 0;
    if (!ready_path || !ready_path[0] || !release_path || !release_path[0])
        return -1;
    FILE *ready = fopen(ready_path, "w");
    if (!ready)
        return -1;
    int failed = fprintf(ready, "%d\n", pid) < 0;
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

struct start_trace {
    int enabled;
    long long started_ms;
    long long previous_ms;
};

static void start_trace_init(struct start_trace *trace)
{
    memset(trace, 0, sizeof(*trace));
    const char *enabled = getenv("HAMN_START_TRACE");
    if (!enabled || strcmp(enabled, "1") != 0)
        return;
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return;
    trace->enabled = 1;
    trace->started_ms = (long long)now.tv_sec * 1000 +
        now.tv_nsec / 1000000;
    trace->previous_ms = trace->started_ms;
}

static void start_trace_stage(struct start_trace *trace, const char *stage)
{
    if (!trace->enabled)
        return;
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return;
    long long current_ms = (long long)now.tv_sec * 1000 +
        now.tv_nsec / 1000000;
    logmsg("trace: hamn-start stage=%s elapsed_ms=%lld delta_ms=%lld",
           stage, current_ms - trace->started_ms,
           current_ms - trace->previous_ms);
    trace->previous_ms = current_ms;
}

struct start_options {
    unsigned cpus;
    unsigned memory_gib;
    unsigned disk_gib;
    int provision;
    int edit;
    int template_enabled;
    const char *flag_profile;
    const char *positional_profile;
};

static void start_usage(FILE *stream)
{
    fprintf(stream,
            "usage: hamn start [-p PROFILE] [PROFILE] [--cpu N] "
            "[--memory GiB] [--disk GiB] [--provision] [--edit] "
            "[--template=false]\n");
}

static int parse_start_options(int argc, char **argv,
                               struct start_options *options)
{
    memset(options, 0, sizeof(*options));
    options->template_enabled = 1;
    static const struct option opts[] = {
        { "cpu", required_argument, NULL, 'c' },
        { "memory", required_argument, NULL, 'm' },
        { "disk", required_argument, NULL, 'd' },
        { "provision", no_argument, NULL, 'P' },
        { "edit", no_argument, NULL, 'e' },
        { "template", required_argument, NULL, 'T' },
        { "profile", required_argument, NULL, 'p' },
        { 0 },
    };
    optind = 1;
    optreset = 1;
    int ch;
    while ((ch = getopt_long(argc, argv, "c:m:d:PeT:p:", opts, NULL)) != -1) {
        switch (ch) {
        case 'c':
            if (profile_parse_positive(optarg, &options->cpus) != 0) {
                logerr("invalid CPU count '%s'", optarg);
                return -1;
            }
            break;
        case 'm':
            if (profile_parse_positive(optarg, &options->memory_gib) != 0 ||
                options->memory_gib > UINT_MAX / 1024U) {
                logerr("invalid memory size '%s'", optarg);
                return -1;
            }
            break;
        case 'd':
            if (profile_parse_positive(optarg, &options->disk_gib) != 0) {
                logerr("invalid disk size '%s'", optarg);
                return -1;
            }
            break;
        case 'P':
            options->provision = 1;
            break;
        case 'e':
            options->edit = 1;
            break;
        case 'T':
            if (strcmp(optarg, "true") == 0)
                options->template_enabled = 1;
            else if (strcmp(optarg, "false") == 0)
                options->template_enabled = 0;
            else {
                logerr("--template must be true or false");
                return -1;
            }
            break;
        case 'p':
            if (options->flag_profile) {
                logerr("profile was specified more than once");
                return -1;
            }
            options->flag_profile = optarg;
            break;
        default:
            start_usage(stderr);
            return -1;
        }
    }
    if (optind + 1 < argc) {
        start_usage(stderr);
        return -1;
    }
    if (optind < argc)
        options->positional_profile = argv[optind];
    if (options->edit && !options->template_enabled) {
        logerr("--edit cannot be combined with --template=false");
        return -1;
    }
    return 0;
}

static int profile_config_exists(const struct profile *profile)
{
    char path[1024];
    struct stat status;
    if (!profile_path(profile, "config.yaml", path, sizeof(path)))
        return -1;
    if (lstat(path, &status) != 0)
        return errno == ENOENT ? 0 : -1;
    return S_ISREG(status.st_mode) ? 1 : -1;
}

static int edit_profile_template(const struct profile *profile)
{
    char config[1024];
    if (!profile_path(profile, "config.yaml", config, sizeof(config)))
        return -1;
    const char *editor = getenv("EDITOR");
    if (!editor || !editor[0])
        editor = "vi";
    if (strpbrk(editor, " \t\r\n")) {
        logerr("EDITOR must name one executable without arguments");
        return -1;
    }
    const char *command[] = { editor, config, NULL };
    return proc_run(command);
}

/* A released installation already selects its signed guest image during
 * installation.  This recovery path covers a missing local selection (for
 * example, after an interrupted cache cleanup) without ever accepting a
 * stock or unsigned image.  The managed `hamn update` command verifies the
 * release manifest, both artifacts, and installs the selection atomically.
 */
static int ensure_signed_guest_image(char *image, size_t capacity,
                                     int *updated)
{
    if (!updated) {
        errno = EINVAL;
        return -1;
    }
    *updated = 0;
    if (fetch_image_ensure(image, capacity) == 0)
        return 0;
    if (errno != ENOENT)
        return -1;

    const char *invocation = cli_invocation_path();
    if (!invocation || !invocation[0]) {
        logerr("cannot prepare the signed guest image: Hamn invocation is unavailable");
        return -1;
    }

    logmsg("preparing the signed Hamn guest image (first start only) ...");
    const char *command[] = { invocation, "update", NULL };
    if (proc_run(command) != 0) {
        logerr("cannot prepare the signed guest image; run 'hamn update' to see the verification error");
        return -1;
    }
    if (fetch_image_ensure(image, capacity) != 0) {
        logerr("the signed update completed without selecting a usable guest image");
        return -1;
    }
    *updated = 1;
    return 0;
}

#ifdef HAMN_TEST
int hamn_test_start_ensure_signed_guest_image(char *image, size_t capacity,
                                              int *updated)
{
    return ensure_signed_guest_image(image, capacity, updated);
}
#endif

static int cmd_start_locked(const struct start_options *options,
                            const char *profile_name)
{
    struct start_trace trace;
    start_trace_init(&trace);
    struct profile p;
    if (profile_load(&p, profile_name) != 0) {
        if (errno == EPROTONOSUPPORT) {
            logerr("profile %s has a removed runtime setting; Hamn will not "
                   "convert it. Recreate it with: hamn delete --data "
                   "--profile %s (confirm y), then hamn start --profile %s",
                   profile_name, profile_name, profile_name);
            return 1;
        }
        die("cannot initialize profile directory");
    }
    int config_exists = profile_config_exists(&p);
    if (config_exists < 0)
        die("cannot inspect profile configuration");
    if (options->edit) {
        if (profile_save(&p) != 0 || edit_profile_template(&p) != 0)
            die("cannot edit the profile template");
        if (profile_load(&p, profile_name) != 0)
            die("edited profile configuration is invalid");
        config_exists = 1;
    }
    char deleted_marker[1024];
    if (!profile_path(&p, "deleted", deleted_marker, sizeof(deleted_marker)) ||
        fs_unlink_if_exists(deleted_marker) != 0)
        die("cannot reactivate profile data");
    int mutation_fd = profile_mutation_lock(&p);
    if (mutation_fd < 0) {
        logerr("another %s profile mutation is running", p.name);
        return 1;
    }
    if (vm_process_wait_spawn_transition(&p, 50) != 0) {
        logerr("an existing vmrun spawn is still in progress or uncertain");
        goto out;
    }
    struct profile running_profile = p;
    enum vm_process_state process_state = vm_process_probe(&p, NULL);
    if (process_state == VM_PROCESS_UNVERIFIED) {
        logerr("cannot verify the existing vmrun process; refusing to "
               "start or signal it");
        goto out;
    }
    int already_running = process_state == VM_PROCESS_VERIFIED;
    if (options->disk_gib && options->disk_gib < p.disk_gib) {
        logerr("disk size cannot shrink (current: %u GiB)", p.disk_gib);
        goto out;
    }
    if (options->cpus)
        p.cpus = options->cpus;
    if (options->memory_gib)
        p.mem_mib = options->memory_gib * 1024;
    if (options->disk_gib)
        p.disk_gib = options->disk_gib;
    int deployment_current = options->provision ? 0 :
        guest_deployment_is_current(&p);
    if (deployment_current < 0) {
        logerr("cannot validate guest deployment marker: %s",
               strerror(errno));
        goto out;
    }
    start_trace_stage(&trace, "profile-ready");
    if (already_running) {
        if (running_profile.cpus != p.cpus ||
            running_profile.mem_mib != p.mem_mib ||
            running_profile.disk_gib != p.disk_gib) {
            logerr("resource change requires a stopped VM; run: hamn stop");
            goto out;
        }
        struct vm_state running_state;
        if (state_load(&p, &running_state) != 0) {
            logerr("cannot load the running VM state");
            goto out;
        }
        if (running_state.ip[0]) {
            if (deployment_current == 0 &&
                guest_deployment_refresh_locked(&p, &running_state) != 0) {
                logerr("cannot refresh the running VM guest deployment");
                goto out;
            }
            if (running_start_ready(&running_profile)) {
                /* Do not stop a healthy VM merely because a host Docker CLI
                 * context must be retried. setup failure returns nonzero with
                 * the VM and its forwarding state intact. */
                if (retry_running_docker_context(&p, &running_state) != 0) {
                    logerr("cannot activate Docker context for the already-running VM");
                    goto out;
                }
                if (docker_observer_start(&p, &running_state) != 0) {
                    logerr("cannot start the Docker port observer");
                    goto out;
                }
                if (mount_inotify_start(&p) != 0) {
                    logerr("cannot start the experimental mountInotify bridge");
                    goto out;
                }
                start_trace_stage(&trace, "already-running");
                logmsg("hamn is already running (profile %s)", p.name);
                profile_mutation_unlock(mutation_fd);
                return 0;
            }
        }
        logmsg("running VM is not ready; recovering the incomplete start");
        if (rollback_incomplete_start(&running_profile) != 0)
            goto out;
    } else if (vm_cleanup_stale(&running_profile) != 0) {
        logerr("cannot clean stale VM state before start");
        goto out;
    }
    if ((config_exists || options->template_enabled || options->cpus ||
         options->memory_gib || options->disk_gib) && profile_save(&p) != 0)
        die("cannot save profile config");

    /* 1. 이미지 + 디스크 */
    char cache_img[1024];
    int signed_image_updated = 0;
    if (ensure_signed_guest_image(cache_img, sizeof(cache_img),
                                  &signed_image_updated) != 0)
        goto out;
    if (signed_image_updated) {
        logmsg("signed guest image is ready; restarting start with the installed Hamn version");
        profile_mutation_unlock(mutation_fd);
        return START_REEXEC_AFTER_SIGNED_UPDATE;
    }
    if (disk_prepare(&p, cache_img) != 0)
        goto out;
    start_trace_stage(&trace, "storage-ready");

    /* 2. ssh 키 + MAC */
    if (ssh_keys_ensure(&p) != 0)
        goto out;
    char mac[32];
    if (mac_ensure(&p, mac, sizeof(mac)) != 0)
        goto out;
    /* 3. 공유/로그 디렉토리. Guest runtime itself is image-owned. */
    char logs[PATH_MAX], home_real[PATH_MAX];
    char vm_shares[VZ_MAX_SHARES][PATH_MAX + 32];
    int vm_share_read_only[VZ_MAX_SHARES] = {0};
    size_t vm_share_count = 0;
    profile_path(&p, "logs", logs, sizeof(logs));
    if (fs_mkdirs(logs, 0755) != 0)
        goto out;
    const char *home = getenv("HOME");
    if (!home) {
        logerr("HOME is not set");
        goto out;
    }
    if (p.mount_home) {
        if (canonical_owned_directory(home, home_real, 0) != 0 ||
            append_vm_share(vm_shares, vm_share_read_only, &vm_share_count,
                            "home", home_real, p.home_read_only) != 0) {
            logerr("cannot prepare the home mount: %s", strerror(errno));
            goto out;
        }
    } else if (!realpath(home, home_real)) {
        logerr("cannot resolve HOME: %s", strerror(errno));
        goto out;
    }
    for (size_t index = 0; index < p.mount_count; index++) {
        char source[PATH_MAX], tag[32];
        const struct profile_mount *mount = &p.mounts[index];
        if (canonical_owned_directory(mount->location, source, 1) != 0 ||
            (mount->writable && !path_is_within(home_real, source))) {
            logerr("mount %s must be an owned non-symlink directory%s",
                   mount->location, mount->writable ? " beneath HOME" : "");
            goto out;
        }
        int written = snprintf(tag, sizeof(tag), "mount%zu", index);
        if (written < 0 || written >= (int)sizeof(tag) ||
            append_vm_share(vm_shares, vm_share_read_only, &vm_share_count,
                            tag, source, !mount->writable) != 0) {
            logerr("cannot prepare custom mount %s", mount->location);
            goto out;
        }
    }
    if (cloudinit_seed_ensure(&p, options->provision) != 0)
        goto out;
    start_trace_stage(&trace, deployment_current == 1 ?
                      "guest-image-configuration-reused" :
                      "guest-image-configuration-required");

    /* 4. vmrun spawn */
    char self[PATH_MAX];
    if (!proc_self_path(self, sizeof(self)))
        die("cannot resolve own executable path");

    char disk[1024], seed[1024], serial[1024], efivars[1024], mid[1024];
    char pidf[1024], identity[1024], spawning[1024], ctl[1024], vmlog[1024];
    char cpus_s[16], mem_s[16], guard_fd_s[16];
    const char *rosetta = p.rosetta ? "true" : "false";
    const char *nested_virtualization = p.nested_virtualization ? "true" :
        "false";
    char spawn_token[VM_SPAWN_TOKEN_HEX_SIZE];

    profile_path(&p, "disk.img", disk, sizeof(disk));
    profile_path(&p, "seed.iso", seed, sizeof(seed));
    profile_path(&p, "logs/serial.log", serial, sizeof(serial));
    profile_path(&p, "efi-vars.bin", efivars, sizeof(efivars));
    profile_path(&p, "machine-id.bin", mid, sizeof(mid));
    profile_path(&p, "vmrun.pid", pidf, sizeof(pidf));
    profile_path(&p, "vmrun.identity", identity, sizeof(identity));
    profile_path(&p, "vmrun.spawning", spawning, sizeof(spawning));
    profile_path(&p, "vmrun.sock", ctl, sizeof(ctl));
    profile_path(&p, "logs/vmrun.log", vmlog, sizeof(vmlog));
    snprintf(cpus_s, sizeof(cpus_s), "%u", p.cpus);
    snprintf(mem_s, sizeof(mem_s), "%u", p.mem_mib);

    struct vm_state starting_state;
    if (state_load(&p, &starting_state) != 0) {
        logerr("cannot load VM state before startup");
        goto rollback;
    }
    snprintf(starting_state.state, sizeof(starting_state.state), "starting");
    starting_state.ip[0] = '\0';
    starting_state.started_at = 0;
    if (state_save(&p, &starting_state) != 0) {
        logerr("cannot persist starting VM state");
        goto rollback;
    }

    int spawn_guard_fd = -1;
    if (vm_spawn_guard_create(&p, &spawn_guard_fd, spawn_token) != 0) {
        logerr("cannot persist vmrun spawn guard");
        goto rollback;
    }
    snprintf(guard_fd_s, sizeof(guard_fd_s), "%d", spawn_guard_fd);

    const char *vmargv[60 + VZ_MAX_SHARES * 2];
    size_t vmargc = 0;
    const char *const vm_prefix[] = {
        self, "vmrun", "--disk", disk, "--seed", seed,
        "--serial-log", serial, "--efi-vars", efivars,
        "--machine-id", mid, "--mac", mac, "--cpus", cpus_s,
        "--mem-mib", mem_s,
        "--rosetta", rosetta,
        "--nested-virtualization", nested_virtualization, NULL,
    };
    for (size_t index = 0; vm_prefix[index]; index++) {
        if (append_vm_argument(vmargv, &vmargc,
                               sizeof(vmargv) / sizeof(vmargv[0]),
                               vm_prefix[index]) != 0)
            die("cannot construct vmrun command");
    }
    for (size_t index = 0; index < vm_share_count; index++) {
        if (append_vm_argument(vmargv, &vmargc,
                               sizeof(vmargv) / sizeof(vmargv[0]),
                               vm_share_read_only[index] ? "--share-ro" : "--share") != 0 ||
            append_vm_argument(vmargv, &vmargc,
                               sizeof(vmargv) / sizeof(vmargv[0]),
                               vm_shares[index]) != 0)
            die("cannot construct vmrun mount arguments");
    }
    const char *const vm_suffix[] = {
        "--pidfile", pidf, "--identity-file", identity,
        "--spawn-guard", spawning, "--spawn-guard-fd", guard_fd_s,
        "--spawn-token", spawn_token, "--ctl-sock", ctl, NULL,
    };
    for (size_t index = 0; vm_suffix[index]; index++) {
        if (append_vm_argument(vmargv, &vmargc,
                               sizeof(vmargv) / sizeof(vmargv[0]),
                               vm_suffix[index]) != 0)
            die("cannot construct vmrun command");
    }
    vmargv[vmargc] = NULL;

    logmsg("starting vm (cpus=%u memory=%uMiB disk=%uGiB) ...", p.cpus,
           p.mem_mib, p.disk_gib);
    pid_t vmrun_pid = proc_spawn_daemon(vmargv, vmlog);
    if (vmrun_pid < 0) {
        logerr("cannot spawn vmrun");
        (void)vm_spawn_guard_release(&p, spawn_guard_fd, 1);
        spawn_guard_fd = -1;
        goto rollback;
    }
    if (vm_spawn_guard_release(&p, spawn_guard_fd, 0) != 0) {
        logerr("cannot hand off vmrun spawn guard");
        spawn_guard_fd = -1;
        goto out;
    }
    spawn_guard_fd = -1;
    const char *test_exit_before_identity =
        getenv("HAMN_TEST_VMRUN_EXIT_BEFORE_IDENTITY");
    if (test_exit_before_identity &&
        strcmp(test_exit_before_identity, "1") == 0 &&
        vm_process_wait_spawn_transition(&p, 50) != 0) {
        logerr("test vmrun did not exit before child identity capture");
        goto out;
    }
    if (test_pre_capture_barrier(vmrun_pid) != 0) {
        logerr("vmrun pre-capture test barrier failed; preserving state");
        goto out;
    }
    struct vm_spawned_process spawned;
    enum vm_spawn_process_result spawn_result =
        vm_process_capture_spawned((int)vmrun_pid, &spawned);
    if (spawn_result == VM_SPAWN_PROCESS_EXITED) {
        logerr("vmrun exited before persisting its startup identity");
        goto rollback;
    }
    if (spawn_result != VM_SPAWN_PROCESS_READY) {
        logerr("cannot capture exact vmrun child identity; preserving state");
        goto out;
    }
    int identity_timeout = test_identity_timeout_dsec();
    if (identity_timeout < 0) {
        logerr("invalid HAMN_TEST_VMRUN_IDENTITY_TIMEOUT_DSEC");
        if (vm_process_abort_spawned(&spawned, 30) != 0)
            goto out;
        goto rollback;
    }
    spawn_result = vm_process_wait_spawned(&p, &spawned,
                                           identity_timeout);
    if (spawn_result == VM_SPAWN_PROCESS_EXITED) {
        logerr("vmrun exited before persisting its startup identity");
        goto rollback;
    }
    if (spawn_result != VM_SPAWN_PROCESS_READY) {
        logerr("vmrun did not persist its startup process identity");
        if (vm_process_abort_spawned(&spawned, 30) != 0) {
            logerr("cannot confirm failed vmrun child termination; "
                   "preserving startup state");
            goto out;
        }
        goto rollback;
    }

    if (wait_vmrun_running(&p, ctl, 20) != 0) {
        logerr("vm did not start; check %s", vmlog);
        goto rollback;
    }
    start_trace_stage(&trace, "vm-running");

    /* 5. 게스트 IP */
    logmsg("waiting for guest ip ...");
    char ip[64];
    int ip_ready = dhcp_wait_ip(mac, 120, ip, sizeof(ip));
    if (ip_ready != 0) {
        logerr("timed out waiting for dhcp lease; check %s", serial);
        goto rollback;
    }
    logmsg("guest ip: %s", ip);
    start_trace_stage(&trace, "guest-ip-ready");

    struct vm_state st;
    if (state_load(&p, &st) != 0) {
        logerr("cannot load VM state after start");
        goto rollback;
    }
    snprintf(st.state, sizeof(st.state), "running");
    snprintf(st.ip, sizeof(st.ip), "%s", ip);
    st.started_at = (long long)time(NULL);
    if (state_save(&p, &st) != 0) {
        logerr("cannot persist running VM state");
        goto rollback;
    }

    /* 6. ssh (첫 부팅은 cloud-init 사용자 생성까지 대기) */
    logmsg("waiting for ssh ...");
    if (ssh_master_start(&p, ip, 180) != 0) {
        logerr("ssh did not come up; check %s", serial);
        goto rollback;
    }
    start_trace_stage(&trace, "ssh-ready");

    if (provision_run_stage(&p, ip, "system") != 0 ||
        provision_run_stage(&p, ip, "user") != 0)
        goto rollback;

    /* 7. stale 배포의 cloud-init 대기는 refresh transaction이 담당한다. */
    start_trace_stage(&trace, deployment_current == 1 ?
                      "guest-deployment-reused" :
                      "guest-deployment-refresh-required");

    /* 8. 동일 fingerprint도 현재 guest gateway와 Docker 설정을 원자적으로
     * 재확인한다. DHCP 주소가 바뀌어도 host.docker.internal과 Docker의
     * host-gateway가 이전 boot 값을 유지하지 않게 한다. */
    if (deployment_current == 1) {
        if (guest_deployment_reconcile_runtime_locked(&p, &st) == 0) {
            logmsg("warm start reused current guest deployment");
        } else {
            logmsg("warm start reconciliation failed; repairing guest deployment");
            if (guest_deployment_repair_locked(&p, &st) != 0)
                goto rollback;
        }
    } else if ((options->provision ? guest_deployment_repair_locked(&p, &st) :
                 guest_deployment_refresh_locked(&p, &st)) != 0) {
        goto rollback;
    }
    start_trace_stage(&trace, "runtime-ready");

    if (provision_run_stage(&p, ip, "after-boot") != 0 ||
        provision_run_stage(&p, ip, "ready") != 0)
        goto rollback;

    char agent_sock[1024];
    profile_path(&p, "agent.sock", agent_sock, sizeof(agent_sock));
    logmsg("agent is up: %s", agent_sock);

    /* 9. Docker socket context */
    char dsock[1024];
    profile_path(&p, "docker.sock", dsock, sizeof(dsock));
    if (setup_docker_context(&p, &st, dsock) != 0)
        goto out;
    logmsg("Docker API is up: %s", dsock);
    if (docker_observer_start(&p, &st) != 0) {
        logerr("cannot start the Docker port observer");
        goto rollback;
    }
    if (mount_inotify_start(&p) != 0) {
        logerr("cannot start the experimental mountInotify bridge");
        goto rollback;
    }
    profile_mutation_unlock(mutation_fd);
    start_trace_stage(&trace, "complete");
    logmsg("done");
    return 0;

rollback:
    rollback_incomplete_start(&p);
out:
    profile_mutation_unlock(mutation_fd);
    return 1;
}

int cmd_start(int argc, char **argv)
{
    struct start_options options;
    if (parse_start_options(argc, argv, &options) != 0)
        return 2;
    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(options.flag_profile, options.positional_profile,
                             profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }
    struct vm_lifecycle_lock lock;
    if (vm_lifecycle_lock_acquire(profile_name, &lock) != 0) {
        logerr("cannot lock the %s profile lifecycle", profile_name);
        return 1;
    }
    int rc = cmd_start_locked(&options, profile_name);
    vm_lifecycle_lock_release(&lock);
    if (rc == START_REEXEC_AFTER_SIGNED_UPDATE) {
        const char *invocation = cli_invocation_path();
        if (!invocation || !invocation[0]) {
            logerr("signed guest image is ready, but Hamn cannot restart itself");
            return 1;
        }
        char **restart = calloc((size_t)argc + 2, sizeof(*restart));
        if (!restart) {
            logerr("signed guest image is ready, but Hamn cannot allocate its restart command");
            return 1;
        }
        restart[0] = (char *)invocation;
        for (int index = 0; index < argc; index++)
            restart[index + 1] = argv[index];
        execvp(restart[0], restart);
        logerr("signed guest image is ready, but Hamn cannot restart: %s",
               strerror(errno));
        free(restart);
        return 1;
    }
    return rc;
}
