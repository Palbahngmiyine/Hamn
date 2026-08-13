#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "sshmgr/ssh.h"
#include "util/proc.h"

int ssh_base_argv(const struct profile *p, const char *argv[], int cap,
                  struct ssh_strbuf *sb)
{
    profile_path(p, "id_ed25519", sb->key, sizeof(sb->key));
    char ctl_path[1000];
    profile_path(p, "ssh.sock", ctl_path, sizeof(ctl_path));
    snprintf(sb->ctl, sizeof(sb->ctl), "ControlPath=%s", ctl_path);

    int n = 0;
    const char *base[] = {
        "ssh",
        "-F", "none",
        "-i", sb->key,
        "-o", "IdentitiesOnly=yes",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "StrictHostKeyChecking=no",
        "-o", "LogLevel=ERROR",
        "-o", "ConnectTimeout=5",
        "-o", "StreamLocalBindUnlink=yes",
        "-o", "StreamLocalBindMask=0177",
        "-o", sb->ctl,
    };
    int bn = (int)(sizeof(base) / sizeof(base[0]));
    if (bn + (p && p->ssh_agent ? 1 : 0) >= cap)
        return -1;
    for (; n < bn; n++)
        argv[n] = base[n];
    /* Agent forwarding is opt-in per profile. It reaches the SSH guest
     * session only; Hamn never bind-mounts an agent socket into containers. */
    if (p && p->ssh_agent)
        argv[n++] = "-A";
    return n;
}

int ssh_master_alive(const struct profile *p)
{
    const char *argv[SSH_ARGV_MAX];
    struct ssh_strbuf sb;
    int n = ssh_base_argv(p, argv, SSH_ARGV_MAX - 4, &sb);
    if (n < 0)
        return -1;
    argv[n++] = "-O";
    argv[n++] = "check";
    argv[n++] = "hamn@placeholder"; /* -O check는 목적지를 쓰지 않음 */
    argv[n] = NULL;

    char out[256];
    return proc_run_capture(argv, out, sizeof(out)) == 0 ? 0 : -1;
}

void ssh_master_exit(const struct profile *p)
{
    const char *argv[SSH_ARGV_MAX];
    struct ssh_strbuf sb;
    int n = ssh_base_argv(p, argv, SSH_ARGV_MAX - 4, &sb);
    if (n < 0)
        return;
    argv[n++] = "-O";
    argv[n++] = "exit";
    argv[n++] = "hamn@placeholder";
    argv[n] = NULL;

    char out[256];
    proc_run_capture(argv, out, sizeof(out));
}

static int try_master_once(const struct profile *p, const char *ip)
{
    const char *argv[SSH_ARGV_MAX];
    struct ssh_strbuf sb;
    char dest[128];
    int n = ssh_base_argv(p, argv, SSH_ARGV_MAX - 10, &sb);
    if (n < 0)
        return -1;
    argv[n++] = "-o";
    argv[n++] = "ControlMaster=yes";
    argv[n++] = "-o";
    argv[n++] = "ControlPersist=no";
    argv[n++] = "-o";
    argv[n++] = "ServerAliveInterval=30";
    argv[n++] = "-N";
    argv[n++] = "-f";
    snprintf(dest, sizeof(dest), "%s@%s", SSH_USER, ip);
    argv[n++] = dest;
    argv[n] = NULL;

    char out[256];
    return proc_run_capture(argv, out, sizeof(out)) == 0 ? 0 : -1;
}

int ssh_master_start(const struct profile *p, const char *ip, int timeout_sec)
{
    if (ssh_master_alive(p) == 0)
        return 0;

    /* 첫 부팅은 cloud-init이 사용자/키를 만들 때까지 거부될 수 있다 */
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return -1;
    long long deadline = (long long)now.tv_sec * 1000 +
        now.tv_nsec / 1000000 + (long long)timeout_sec * 1000;
    long delay_ms = 100;
    for (;;) {
        if (try_master_once(p, ip) == 0)
            return 0;
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
            return -1;
        long long current = (long long)now.tv_sec * 1000 +
            now.tv_nsec / 1000000;
        if (current >= deadline)
            return -1;
        long long remaining = deadline - current;
        long sleep_ms = delay_ms < remaining ? delay_ms : (long)remaining;
        struct timespec delay = {
            .tv_sec = sleep_ms / 1000,
            .tv_nsec = (sleep_ms % 1000) * 1000000,
        };
        while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {}
        if (delay_ms < 500)
            delay_ms *= 2;
    }
}

static int remote_word_is_shell_safe(const char *word)
{
    if (!word || !*word)
        return 0;
    for (const unsigned char *cursor = (const unsigned char *)word; *cursor;
         cursor++) {
        unsigned char ch = *cursor;
        if (!((ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
              (ch >= '0' && ch <= '9') || strchr("_@%+=:,./-", ch)))
            return 0;
    }
    return 1;
}

/*
 * OpenSSH joins all arguments after the destination into one remote shell
 * command. Build that command ourselves and quote every unsafe word so a
 * value supplied by a caller can never become shell syntax in the guest.
 */
static char *remote_command_build(const char *const remote_argv[])
{
    if (!remote_argv || !remote_argv[0]) {
        errno = EINVAL;
        return NULL;
    }
    size_t needed = 1;
    for (size_t i = 0; remote_argv[i]; i++) {
        const char *word = remote_argv[i];
        size_t word_size = remote_word_is_shell_safe(word) ? strlen(word) : 2;
        if (!remote_word_is_shell_safe(word)) {
            for (const char *cursor = word; *cursor; cursor++)
                word_size += *cursor == '\'' ? 4 : 1;
        }
        if (word_size > SIZE_MAX - needed - (i ? 1 : 0)) {
            errno = EOVERFLOW;
            return NULL;
        }
        needed += word_size + (i ? 1 : 0);
    }
    char *command = malloc(needed);
    if (!command)
        return NULL;
    char *out = command;
    for (size_t i = 0; remote_argv[i]; i++) {
        const char *word = remote_argv[i];
        if (i)
            *out++ = ' ';
        if (remote_word_is_shell_safe(word)) {
            size_t length = strlen(word);
            memcpy(out, word, length);
            out += length;
            continue;
        }
        *out++ = '\'';
        for (const char *cursor = word; *cursor; cursor++) {
            if (*cursor == '\'') {
                memcpy(out, "'\\''", 4);
                out += 4;
            } else {
                *out++ = *cursor;
            }
        }
        *out++ = '\'';
    }
    *out = '\0';
    return command;
}

int ssh_exec(const struct profile *p, const char *ip,
             const char *const remote_argv[], int quiet)
{
    const char *argv[SSH_ARGV_MAX];
    struct ssh_strbuf sb;
    char dest[128];
    int n = ssh_base_argv(p, argv, SSH_ARGV_MAX, &sb);
    if (n < 0)
        return -1;
    snprintf(dest, sizeof(dest), "%s@%s", SSH_USER, ip);
    argv[n++] = dest;
    char *command = remote_command_build(remote_argv);
    if (!command)
        return -1;
    argv[n++] = command;
    argv[n] = NULL;

    int rc;
    if (quiet) {
        char out[1024];
        rc = proc_run_capture(argv, out, sizeof(out));
    } else {
        rc = proc_run(argv);
    }
    free(command);
    return rc;
}

int ssh_exec_capture(const struct profile *p, const char *ip,
                     const char *const remote_argv[], char *out, size_t cap)
{
    return ssh_exec_capture_checked(p, ip, remote_argv, out, cap, NULL);
}

int ssh_exec_capture_checked(const struct profile *p, const char *ip,
                             const char *const remote_argv[], char *out,
                             size_t cap, int *truncated)
{
    const char *argv[SSH_ARGV_MAX];
    struct ssh_strbuf sb;
    char dest[128];
    int n = ssh_base_argv(p, argv, SSH_ARGV_MAX, &sb);
    if (n < 0)
        return -1;
    snprintf(dest, sizeof(dest), "%s@%s", SSH_USER, ip);
    argv[n++] = dest;
    char *command = remote_command_build(remote_argv);
    if (!command)
        return -1;
    argv[n++] = command;
    argv[n] = NULL;
    int rc = proc_run_capture_checked(argv, out, cap, truncated);
    free(command);
    return rc;
}
