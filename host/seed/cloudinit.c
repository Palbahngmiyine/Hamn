#include "seed/cloudinit.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "core/log.h"
#include "sshmgr/ssh.h"
#include "util/fs.h"
#include "util/proc.h"

/*
 * dhcp-identifier: mac — systemd-networkd가 기본값(RFC 4361 DUID) 대신
 * MAC을 DHCP 클라이언트 ID로 보내게 한다. macOS bootpd의
 * /var/db/dhcpd_leases가 hw_address에 클라이언트 ID를 기록하므로,
 * 이것 없이는 호스트가 MAC으로 게스트 IP를 찾을 수 없다 (lima와 동일 처리).
 */
static const char NETWORK_CONFIG[] =
    "version: 2\n"
    "ethernets:\n"
    "  hamnnet:\n"
    "    match:\n"
    "      name: \"en*\"\n"
    "    dhcp4: true\n"
    "    dhcp-identifier: mac\n";

/*
 * user-data: hamn 사용자와 virtiofs mount만 준비한다. Docker, system
 * containerd, runc, CNI, binfmt와 hamnd는 signed Ubuntu guest image에 이미
 * 들어 있어야 한다. cloud-init network package install은 release runtime의
 * 구성요소나 trust boundary를 바꾸지 않는다.
 */
static const char USER_DATA_HEADER[] =
    "#cloud-config\n"
    "hostname: hamn\n"
    "users:\n"
    "  - name: hamn\n"
    "    groups: [sudo, hamn]\n"
    "    sudo: \"ALL=(ALL) NOPASSWD:ALL\"\n"
    "    shell: /bin/bash\n"
    "    lock_passwd: true\n"
    "    ssh_authorized_keys:\n"
    "      - ";

#define USER_DATA_CAP (32U * 1024U)

static int append_text(char *output, size_t capacity, size_t *length,
                       const char *format, ...)
{
    if (*length >= capacity)
        return -1;
    va_list arguments;
    va_start(arguments, format);
    int written = vsnprintf(output + *length, capacity - *length,
                            format, arguments);
    va_end(arguments);
    if (written < 0 || (size_t)written >= capacity - *length)
        return -1;
    *length += (size_t)written;
    return 0;
}

static int append_yaml_string(char *output, size_t capacity, size_t *length,
                              const char *value)
{
    if (append_text(output, capacity, length, "\"") != 0)
        return -1;
    for (const unsigned char *cursor = (const unsigned char *)value; *cursor;
         cursor++) {
        switch (*cursor) {
        case '\\':
            if (append_text(output, capacity, length, "\\\\") != 0)
                return -1;
            break;
        case '\"':
            if (append_text(output, capacity, length, "\\\"") != 0)
                return -1;
            break;
        case '\n':
            if (append_text(output, capacity, length, "\\n") != 0)
                return -1;
            break;
        case '\r':
            if (append_text(output, capacity, length, "\\r") != 0)
                return -1;
            break;
        default:
            if (*cursor < 0x20 || append_text(output, capacity, length,
                                               "%c", *cursor) != 0)
                return -1;
        }
    }
    return append_text(output, capacity, length, "\"");
}

static int append_mount(char *output, size_t capacity, size_t *length,
                        const char *tag, const char *mount_point,
                        int read_only)
{
    if (append_text(output, capacity, length, "  - [ ") != 0 ||
        append_yaml_string(output, capacity, length, tag) != 0 ||
        append_text(output, capacity, length, ", ") != 0 ||
        append_yaml_string(output, capacity, length, mount_point) != 0 ||
        append_text(output, capacity, length, ", virtiofs, ") != 0 ||
        append_yaml_string(output, capacity, length,
                           read_only ? "ro,nofail" : "defaults,nofail") != 0 ||
        append_text(output, capacity, length, ", \"0\", \"0\" ]\n") != 0)
        return -1;
    return 0;
}

static int build_user_data(const struct profile *profile, const char *pubkey,
                           const char *home, char output[USER_DATA_CAP])
{
    size_t length = 0;
    if (append_text(output, USER_DATA_CAP, &length, "%s", USER_DATA_HEADER) != 0 ||
        append_yaml_string(output, USER_DATA_CAP, &length, pubkey) != 0 ||
        append_text(output, USER_DATA_CAP, &length, "\nmounts:\n") != 0)
        return -1;
    if (profile->mount_home &&
        append_mount(output, USER_DATA_CAP, &length, "home", home,
                     profile->home_read_only) != 0)
        return -1;
    /* The host supplies the read-only translation directory only after the
     * user opted in. Guest registration remains explicit and is documented,
     * matching Apple's Linux Rosetta activation boundary. */
    if (profile->rosetta &&
        append_mount(output, USER_DATA_CAP, &length, "rosetta",
                     "/mnt/hamn-rosetta", 1) != 0)
        return -1;
    for (size_t index = 0; index < profile->mount_count; index++) {
        char tag[32];
        int written = snprintf(tag, sizeof(tag), "mount%zu", index);
        if (written < 0 || written >= (int)sizeof(tag) ||
            append_mount(output, USER_DATA_CAP, &length, tag,
                         profile->mounts[index].mount_point,
                         !profile->mounts[index].writable) != 0)
            return -1;
    }
    return 0;
}

static int seed_is_current(const struct profile *profile, const char *iso)
{
    struct stat seed;
    if (stat(iso, &seed) != 0)
        return 0;
    char config[1024];
    struct stat profile_config;
    if (!profile_path(profile, "config.yaml", config, sizeof(config)) ||
        stat(config, &profile_config) != 0)
        return 1;
    return profile_config.st_mtimespec.tv_sec < seed.st_mtimespec.tv_sec ||
        (profile_config.st_mtimespec.tv_sec == seed.st_mtimespec.tv_sec &&
         profile_config.st_mtimespec.tv_nsec <= seed.st_mtimespec.tv_nsec);
}

static int write_file(const char *path, const char *content)
{
    FILE *f = fopen(path, "w");
    if (!f)
        return -1;
    fputs(content, f);
    return fclose(f);
}

int cloudinit_seed_ensure(const struct profile *p, int force)
{
    char iso[1024];
    profile_path(p, "seed.iso", iso, sizeof(iso));
    if (!force && seed_is_current(p, iso))
        return 0;

    char pubkey[1024];
    if (ssh_read_pubkey(p, pubkey, sizeof(pubkey)) != 0) {
        logerr("cannot read ssh public key");
        return -1;
    }
    const char *home = getenv("HOME");
    if (!home)
        return -1;

    char tmpdir[1024];
    profile_path(p, "seed.tmp", tmpdir, sizeof(tmpdir));
    if (fs_mkdirs(tmpdir, 0700) != 0)
        return -1;

    char path[1100], buf[USER_DATA_CAP];

    snprintf(path, sizeof(path), "%s/meta-data", tmpdir);
    snprintf(buf, sizeof(buf),
             "instance-id: hamn-%lld\nlocal-hostname: hamn\n",
             (long long)time(NULL));
    if (write_file(path, buf) != 0)
        return -1;

    snprintf(path, sizeof(path), "%s/user-data", tmpdir);
    if (build_user_data(p, pubkey, home, buf) != 0 ||
        write_file(path, buf) != 0)
        return -1;

    snprintf(path, sizeof(path), "%s/network-config", tmpdir);
    if (write_file(path, NETWORK_CONFIG) != 0)
        return -1;

    const char *mk[] = { "hdiutil", "makehybrid", "-quiet", "-iso",
                         "-joliet", "-default-volume-name", "CIDATA",
                         "-ov", "-o", iso, tmpdir, NULL };
    if (proc_run(mk) != 0) {
        logerr("hdiutil makehybrid failed");
        return -1;
    }

    /* seed.tmp 정리 */
    snprintf(path, sizeof(path), "%s/meta-data", tmpdir);
    unlink(path);
    snprintf(path, sizeof(path), "%s/user-data", tmpdir);
    unlink(path);
    snprintf(path, sizeof(path), "%s/network-config", tmpdir);
    unlink(path);
    rmdir(tmpdir);
    return 0;
}
