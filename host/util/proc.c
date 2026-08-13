#include "util/proc.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <libproc.h>
#include <mach-o/dyld.h>
#include <poll.h>
#include <signal.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

static const int forwarded_signals[] = { SIGINT, SIGTERM, SIGHUP };
static volatile sig_atomic_t supervised_process_group = -1;

struct signal_forwarding {
    struct sigaction previous[3];
    int active;
};

int proc_start_identity(pid_t pid, uint64_t *sec, uint64_t *usec)
{
    struct proc_bsdinfo info;
    int size = proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info));
    if (size != (int)sizeof(info) || info.pbi_pid != (uint32_t)pid)
        return -1;
    *sec = info.pbi_start_tvsec;
    *usec = info.pbi_start_tvusec;
    return 0;
}

int proc_executable_identity(pid_t pid, unsigned char uuid[16])
{
    struct rusage_info_v0 usage;
    memset(&usage, 0, sizeof(usage));
    if (proc_pid_rusage(pid, RUSAGE_INFO_V0,
                        (rusage_info_t *)&usage) != 0)
        return -1;
    unsigned char any = 0;
    for (int i = 0; i < 16; i++)
        any |= usage.ri_uuid[i];
    if (any == 0)
        return -1;
    memcpy(uuid, usage.ri_uuid, 16);
    return 0;
}

void proc_executable_uuid_format(const unsigned char uuid[16], char hex[33])
{
    static const char digits[] = "0123456789abcdef";
    for (int i = 0; i < 16; i++) {
        hex[i * 2] = digits[uuid[i] >> 4];
        hex[i * 2 + 1] = digits[uuid[i] & 0x0f];
    }
    hex[32] = '\0';
}

struct terminal_state {
    int fd;
    pid_t owner_group;
    pid_t previous_foreground_group;
    struct termios attributes;
    int active;
};

static int terminal_foreground_set(int fd, pid_t process_group)
{
    sigset_t signals, previous_mask;
    sigemptyset(&signals);
    sigaddset(&signals, SIGTTOU);
    if (sigprocmask(SIG_BLOCK, &signals, &previous_mask) != 0)
        return -1;
    int rc = tcsetpgrp(fd, process_group);
    int saved = errno;
    if (sigprocmask(SIG_SETMASK, &previous_mask, NULL) != 0)
        abort();
    errno = saved;
    return rc;
}

static int terminal_state_prepare(struct terminal_state *terminal)
{
    memset(terminal, 0, sizeof(*terminal));
    terminal->fd = STDIN_FILENO;
    if (!isatty(STDIN_FILENO) || !isatty(STDOUT_FILENO)) {
        errno = ENOTTY;
        return -1;
    }
    terminal->owner_group = getpgrp();
    terminal->previous_foreground_group = tcgetpgrp(terminal->fd);
    if (terminal->previous_foreground_group < 0 ||
        terminal->previous_foreground_group != terminal->owner_group) {
        errno = EPERM;
        return -1;
    }
    if (tcgetattr(terminal->fd, &terminal->attributes) != 0)
        return -1;
    terminal->active = 1;
    return 0;
}

static int terminal_state_restore(struct terminal_state *terminal)
{
    if (!terminal->active)
        return 0;
    int rc = terminal_foreground_set(terminal->fd,
                                     terminal->previous_foreground_group);
    if (tcsetattr(terminal->fd, TCSADRAIN, &terminal->attributes) != 0)
        rc = -1;
    return rc;
}

static pid_t terminal_wait(pid_t supervisor, int *status,
                           struct terminal_state *terminal)
{
    for (;;) {
        pid_t waited;
        do {
            waited = waitpid(supervisor, status, WUNTRACED);
        } while (waited < 0 && errno == EINTR);
        if (waited != supervisor) {
            fprintf(stderr, "cannot wait for terminal job: %s\n",
                    strerror(errno));
            return waited;
        }
        if (!WIFSTOPPED(*status))
            return waited;

        if (terminal_state_restore(terminal) != 0) {
            fprintf(stderr, "cannot restore terminal after job stop: %s\n",
                    strerror(errno));
            return -1;
        }
        if (kill(0, SIGTSTP) != 0) {
            fprintf(stderr, "cannot stop terminal owner: %s\n",
                    strerror(errno));
            return -1;
        }
        if (terminal_foreground_set(terminal->fd, supervisor) != 0) {
            fprintf(stderr, "cannot return terminal to job: %s\n",
                    strerror(errno));
            return -1;
        }
        if (kill(-supervisor, SIGCONT) != 0) {
            fprintf(stderr, "cannot resume terminal job: %s\n",
                    strerror(errno));
            return -1;
        }
    }
}

static void forward_supervised_signal(int signal_number)
{
    pid_t process_group = (pid_t)supervised_process_group;
    if (process_group > 0)
        (void)kill(-process_group, signal_number);
}

static void guarded_signal_set(sigset_t *signals)
{
    sigemptyset(signals);
    sigaddset(signals, SIGINT);
    sigaddset(signals, SIGTERM);
    sigaddset(signals, SIGHUP);
}

static int signal_forwarding_install(pid_t process_group,
                                     struct signal_forwarding *forwarding)
{
    memset(forwarding, 0, sizeof(*forwarding));
    if (supervised_process_group > 0) {
        errno = EBUSY;
        return -1;
    }
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = forward_supervised_signal;
    sigemptyset(&action.sa_mask);
    size_t installed = 0;
    for (; installed < sizeof(forwarded_signals) /
                           sizeof(forwarded_signals[0]); installed++) {
        if (sigaction(forwarded_signals[installed], &action,
                      &forwarding->previous[installed]) != 0)
            break;
    }
    if (installed != sizeof(forwarded_signals) /
                         sizeof(forwarded_signals[0])) {
        int saved = errno;
        while (installed > 0) {
            installed--;
            (void)sigaction(forwarded_signals[installed],
                            &forwarding->previous[installed], NULL);
        }
        errno = saved;
        return -1;
    }
    supervised_process_group = (sig_atomic_t)process_group;
    forwarding->active = 1;
    return 0;
}

static void signal_forwarding_restore(struct signal_forwarding *forwarding)
{
    if (!forwarding->active)
        return;
    sigset_t signals, previous_mask;
    guarded_signal_set(&signals);
    if (sigprocmask(SIG_BLOCK, &signals, &previous_mask) != 0)
        abort();
    supervised_process_group = -1;
    for (size_t i = 0; i < sizeof(forwarded_signals) /
                               sizeof(forwarded_signals[0]); i++) {
        if (sigaction(forwarded_signals[i], &forwarding->previous[i],
                      NULL) != 0)
            abort();
    }
    forwarding->active = 0;
    if (sigprocmask(SIG_SETMASK, &previous_mask, NULL) != 0)
        abort();
}

static unsigned supervisor_grace_milliseconds(void)
{
    const char *value = getenv("HAMN_TEST_SUPERVISOR_GRACE_MS");
    if (!value)
        return 30 * 1000;
    char *end = NULL;
    errno = 0;
    long milliseconds = strtol(value, &end, 10);
    if (errno || !end || *end || milliseconds < 50 ||
        milliseconds > 30 * 1000)
        return 30 * 1000;
    return (unsigned)milliseconds;
}

static int monotonic_milliseconds(uint64_t *milliseconds)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return -1;
    *milliseconds = (uint64_t)now.tv_sec * 1000 +
                    (uint64_t)now.tv_nsec / 1000000;
    return 0;
}

int proc_run_guarded(const char *const argv[], char *out, size_t cap,
                     int *truncated, int owner_fd, int release_fd,
                     int spawn_ack_fd)
{
    if ((out && cap == 0) || owner_fd < 0)
        return -1;
    int output_pipe[2] = { -1, -1 };
    if (out && pipe(output_pipe) != 0)
        return -1;

    sigset_t guarded_signals, previous_mask;
    guarded_signal_set(&guarded_signals);
    if (sigprocmask(SIG_BLOCK, &guarded_signals, &previous_mask) != 0) {
        if (out) {
            close(output_pipe[0]);
            close(output_pipe[1]);
        }
        return -1;
    }
    pid_t command = fork();
    if (command < 0) {
        (void)sigprocmask(SIG_SETMASK, &previous_mask, NULL);
        if (out) {
            close(output_pipe[0]);
            close(output_pipe[1]);
        }
        return -1;
    }
    if (command == 0) {
        struct sigaction action = { .sa_handler = SIG_DFL };
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGINT, &action, NULL) != 0 ||
            sigaction(SIGTERM, &action, NULL) != 0 ||
            sigaction(SIGHUP, &action, NULL) != 0 ||
            sigaction(SIGPIPE, &action, NULL) != 0 ||
            sigprocmask(SIG_SETMASK, &previous_mask, NULL) != 0)
            _exit(127);
        close(owner_fd);
        if (release_fd >= 0)
            close(release_fd);
        if (spawn_ack_fd >= 0)
            close(spawn_ack_fd);
        if (out) {
            close(output_pipe[0]);
            if (dup2(output_pipe[1], STDOUT_FILENO) < 0 ||
                dup2(output_pipe[1], STDERR_FILENO) < 0)
                _exit(127);
            close(output_pipe[1]);
        }
        execvp(argv[0], (char *const *)argv);
        _exit(127);
    }

    if (sigprocmask(SIG_SETMASK, &previous_mask, NULL) != 0) {
        (void)kill(command, SIGKILL);
        while (waitpid(command, NULL, 0) < 0 && errno == EINTR)
            ;
        if (out) {
            close(output_pipe[0]);
            close(output_pipe[1]);
        }
        return -1;
    }
    if (spawn_ack_fd >= 0) {
        char byte = '1';
        while (write(spawn_ack_fd, &byte, 1) < 0 && errno == EINTR)
            ;
        close(spawn_ack_fd);
    }
    if (release_fd >= 0)
        close(release_fd);
    if (out)
        close(output_pipe[1]);
    if (truncated)
        *truncated = 0;

    size_t offset = 0;
    int output_eof = !out;
    int command_exited = 0;
    int status = 0;
    int read_failed = 0;
    int owner_dead = 0;
    int term_sent = 0;
    int kill_sent = 0;
    uint64_t terminate_at = 0;
    uint64_t kill_at = 0;
    unsigned grace_ms = supervisor_grace_milliseconds();
    while (!command_exited || !output_eof) {
        struct pollfd pfds[2] = {
            {
                .fd = owner_fd,
                .events = POLLIN | POLLHUP,
            },
            {
                .fd = output_eof ? -1 : output_pipe[0],
                .events = POLLIN | POLLHUP,
            },
        };
        int ready;
        do {
            ready = poll(pfds, 2, 50);
        } while (ready < 0 && errno == EINTR);
        if (ready < 0) {
            read_failed = 1;
            output_eof = 1;
        }
        if (!owner_dead && ready > 0 &&
            (pfds[0].revents & (POLLIN | POLLHUP | POLLERR | POLLNVAL))) {
            char byte;
            ssize_t n;
            do {
                n = read(owner_fd, &byte, 1);
            } while (n < 0 && errno == EINTR);
            if (n == 0) {
                uint64_t now = 0;
                if (monotonic_milliseconds(&now) != 0) {
                    (void)kill(command, SIGKILL);
                    kill_sent = 1;
                    read_failed = 1;
                } else {
                    owner_dead = 1;
                    terminate_at = now + grace_ms;
                }
                close(owner_fd);
                owner_fd = -1;
            } else if (n < 0 || (pfds[0].revents & (POLLERR | POLLNVAL))) {
                (void)kill(command, SIGKILL);
                kill_sent = 1;
                read_failed = 1;
                owner_dead = 1;
                close(owner_fd);
                owner_fd = -1;
            }
        }
        if (!output_eof && ready > 0) {
            struct pollfd *pfd = &pfds[1];
            if (pfd->revents & (POLLERR | POLLNVAL)) {
                read_failed = 1;
                output_eof = 1;
            } else if (pfd->revents & (POLLIN | POLLHUP)) {
                char chunk[4096];
                ssize_t n = read(output_pipe[0], chunk, sizeof(chunk));
                if (n < 0 && errno != EINTR) {
                    read_failed = 1;
                    output_eof = 1;
                } else if (n == 0) {
                    output_eof = 1;
                } else if (n > 0) {
                    size_t copy = (size_t)n;
                    if (offset >= cap - 1)
                        copy = 0;
                    else if (copy > cap - 1 - offset)
                        copy = cap - 1 - offset;
                    if (copy > 0) {
                        memcpy(out + offset, chunk, copy);
                        offset += copy;
                    }
                    if (truncated && copy < (size_t)n)
                        *truncated = 1;
                }
            }
        }

        if (!command_exited) {
            pid_t waited;
            do {
                waited = waitpid(command, &status, WNOHANG);
            } while (waited < 0 && errno == EINTR);
            if (waited == command)
                command_exited = 1;
            else if (waited < 0) {
                command_exited = 1;
                read_failed = 1;
            }
        }
        /*
         * Only the exact child is part of this synchronous operation. If it
         * has exited and no captured bytes are currently readable, do not let
         * an unrelated descendant retain the capture writer and inherited
         * operation flock forever.
         */
        if (owner_dead && command_exited && !output_eof)
            output_eof = 1;
        if (!command_exited && owner_dead) {
            uint64_t now = 0;
            if (monotonic_milliseconds(&now) != 0) {
                (void)kill(command, SIGKILL);
                kill_sent = 1;
                read_failed = 1;
            } else if (!term_sent && now >= terminate_at) {
                (void)kill(command, SIGTERM);
                term_sent = 1;
                kill_at = now + 1000;
            } else if (term_sent && !kill_sent && now >= kill_at) {
                (void)kill(command, SIGKILL);
                kill_sent = 1;
            }
        }
    }
    if (out) {
        close(output_pipe[0]);
        out[offset] = '\0';
        while (offset > 0 &&
               (out[offset - 1] == '\n' || out[offset - 1] == '\r'))
            out[--offset] = '\0';
    }
    if (owner_fd >= 0)
        close(owner_fd);
    if (read_failed || !WIFEXITED(status))
        return -1;
    return WEXITSTATUS(status);
}

struct supervised_result {
    int rc;
    int truncated;
    size_t length;
};

int proc_pipe_cloexec(int fds[2])
{
    if (pipe(fds) != 0)
        return -1;
    if (fcntl(fds[0], F_SETFD, FD_CLOEXEC) != 0 ||
        fcntl(fds[1], F_SETFD, FD_CLOEXEC) != 0) {
        close(fds[0]);
        close(fds[1]);
        return -1;
    }
    return 0;
}

int proc_write_all(int fd, const void *data, size_t length)
{
    const char *cursor = data;
    while (length > 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0)
            return -1;
        cursor += written;
        length -= (size_t)written;
    }
    return 0;
}

int proc_read_all(int fd, void *data, size_t length)
{
    char *cursor = data;
    while (length > 0) {
        ssize_t received = read(fd, cursor, length);
        if (received < 0 && errno == EINTR)
            continue;
        if (received <= 0)
            return -1;
        cursor += received;
        length -= (size_t)received;
    }
    return 0;
}

static int run_supervised(const char *const argv[], char *out, size_t cap,
                          int *truncated, proc_completion_fn completion,
                          void *context, int terminal_mode)
{
    if ((out && cap == 0) || (out && terminal_mode))
        return -1;
    struct terminal_state terminal;
    if (terminal_mode && terminal_state_prepare(&terminal) != 0)
        return -1;
    if (truncated)
        *truncated = 0;
    int owner_pipe[2], result_pipe[2];
    if (proc_pipe_cloexec(owner_pipe) != 0)
        return -1;
    if (proc_pipe_cloexec(result_pipe) != 0) {
        close(owner_pipe[0]);
        close(owner_pipe[1]);
        return -1;
    }
    sigset_t guarded_signals, previous_mask;
    guarded_signal_set(&guarded_signals);
    if (sigprocmask(SIG_BLOCK, &guarded_signals, &previous_mask) != 0) {
        close(owner_pipe[0]);
        close(owner_pipe[1]);
        close(result_pipe[0]);
        close(result_pipe[1]);
        return -1;
    }
    pid_t supervisor = fork();
    if (supervisor < 0) {
        (void)sigprocmask(SIG_SETMASK, &previous_mask, NULL);
        close(owner_pipe[0]);
        close(owner_pipe[1]);
        close(result_pipe[0]);
        close(result_pipe[1]);
        return -1;
    }
    if (supervisor == 0) {
        struct sigaction action = { .sa_handler = SIG_IGN };
        sigemptyset(&action.sa_mask);
        if (setpgid(0, 0) != 0 ||
            sigaction(SIGINT, &action, NULL) != 0 ||
            sigaction(SIGTERM, &action, NULL) != 0 ||
            sigaction(SIGHUP, &action, NULL) != 0 ||
            sigaction(SIGPIPE, &action, NULL) != 0 ||
            sigprocmask(SIG_SETMASK, &previous_mask, NULL) != 0)
            _exit(1);
        close(owner_pipe[1]);
        close(result_pipe[0]);
        char *capture = out ? calloc(cap, 1) : NULL;
        struct supervised_result result = { .rc = -1 };
        if (!out || capture) {
            result.rc = proc_run_guarded(argv, capture, cap,
                                         &result.truncated, owner_pipe[0],
                                         -1, -1);
            if (completion && completion(result.rc, context) != 0)
                result.rc = -1;
            result.length = capture ? strlen(capture) : 0;
        } else {
            close(owner_pipe[0]);
        }
        int failed = (out && result.length >= cap) ||
            proc_write_all(result_pipe[1], &result, sizeof(result)) != 0 ||
            (result.length > 0 &&
             proc_write_all(result_pipe[1], capture, result.length) != 0);
        free(capture);
        close(result_pipe[1]);
        _exit(failed ? 1 : 0);
    }
    struct signal_forwarding forwarding;
    if (setpgid(supervisor, supervisor) != 0 && errno != EACCES) {
        (void)kill(supervisor, SIGKILL);
        (void)sigprocmask(SIG_SETMASK, &previous_mask, NULL);
        close(owner_pipe[0]);
        close(owner_pipe[1]);
        close(result_pipe[0]);
        close(result_pipe[1]);
        while (waitpid(supervisor, NULL, 0) < 0 && errno == EINTR)
            ;
        return -1;
    }
    if (signal_forwarding_install(supervisor, &forwarding) != 0) {
        (void)kill(-supervisor, SIGKILL);
        (void)kill(supervisor, SIGKILL);
        (void)sigprocmask(SIG_SETMASK, &previous_mask, NULL);
        close(owner_pipe[0]);
        close(owner_pipe[1]);
        close(result_pipe[0]);
        close(result_pipe[1]);
        while (waitpid(supervisor, NULL, 0) < 0 && errno == EINTR)
            ;
        return -1;
    }
    close(owner_pipe[0]);
    close(result_pipe[1]);
    if (sigprocmask(SIG_SETMASK, &previous_mask, NULL) != 0) {
        signal_forwarding_restore(&forwarding);
        (void)kill(-supervisor, SIGKILL);
        close(owner_pipe[1]);
        close(result_pipe[0]);
        while (waitpid(supervisor, NULL, 0) < 0 && errno == EINTR)
            ;
        return -1;
    }
    if (terminal_mode &&
        terminal_foreground_set(terminal.fd, supervisor) != 0) {
        signal_forwarding_restore(&forwarding);
        (void)kill(-supervisor, SIGKILL);
        close(owner_pipe[1]);
        close(result_pipe[0]);
        while (waitpid(supervisor, NULL, 0) < 0 && errno == EINTR)
            ;
        (void)terminal_state_restore(&terminal);
        return -1;
    }
    struct supervised_result result = {0};
    int status = 0;
    pid_t waited = 0;
    if (terminal_mode)
        waited = terminal_wait(supervisor, &status, &terminal);
    if (terminal_mode && waited != supervisor) {
        (void)kill(-supervisor, SIGKILL);
        do {
            waited = waitpid(supervisor, &status, 0);
        } while (waited < 0 && errno == EINTR);
    }
    int read_failed = proc_read_all(result_pipe[0], &result,
                                    sizeof(result)) != 0;
    if (!read_failed && out && result.length >= cap)
        read_failed = 1;
    if (!read_failed && result.length > 0 &&
        proc_read_all(result_pipe[0], out, result.length) != 0)
        read_failed = 1;
    close(result_pipe[0]);
    if (!terminal_mode) {
        do {
            waited = waitpid(supervisor, &status, 0);
        } while (waited < 0 && errno == EINTR);
    }
    close(owner_pipe[1]);
    signal_forwarding_restore(&forwarding);
    if (terminal_mode && terminal_state_restore(&terminal) != 0) {
        fprintf(stderr, "cannot restore terminal after job exit: %s\n",
                strerror(errno));
        return -1;
    }
    if (waited != supervisor || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0 || read_failed) {
        if (terminal_mode)
            fprintf(stderr, "interactive supervisor failed\n");
        return -1;
    }
    if (out) {
        out[result.length] = '\0';
        if (truncated)
            *truncated = result.truncated;
    }
    return result.rc;
}

int proc_run(const char *const argv[])
{
    return run_supervised(argv, NULL, 0, NULL, NULL, NULL, 0);
}

int proc_run_terminal(const char *const argv[])
{
    return run_supervised(argv, NULL, 0, NULL, NULL, NULL, 1);
}

int proc_run_capture(const char *const argv[], char *out, size_t cap)
{
    return run_supervised(argv, out, cap, NULL, NULL, NULL, 0);
}

int proc_run_capture_checked(const char *const argv[], char *out, size_t cap,
                             int *truncated)
{
    return run_supervised(argv, out, cap, truncated, NULL, NULL, 0);
}

int proc_run_supervised(const char *const argv[])
{
    return run_supervised(argv, NULL, 0, NULL, NULL, NULL, 0);
}

int proc_run_supervised_callback(const char *const argv[],
                                 proc_completion_fn completion,
                                 void *context)
{
    return run_supervised(argv, NULL, 0, NULL, completion, context, 0);
}

pid_t proc_spawn_daemon(const char *const argv[], const char *logfile)
{
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_addopen(&fa, STDIN_FILENO, "/dev/null", O_RDONLY,
                                     0);
    posix_spawn_file_actions_addopen(&fa, STDOUT_FILENO, logfile,
                                     O_WRONLY | O_CREAT | O_APPEND, 0600);
    posix_spawn_file_actions_adddup2(&fa, STDOUT_FILENO, STDERR_FILENO);

    posix_spawnattr_t attr;
    posix_spawnattr_init(&attr);
    posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETSID);

    pid_t pid = -1;
    int rc = posix_spawnp(&pid, argv[0], &fa, &attr,
                          (char *const *)(uintptr_t)argv, environ);
    posix_spawn_file_actions_destroy(&fa);
    posix_spawnattr_destroy(&attr);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
    return pid;
}

const char *proc_self_path(char *buf, size_t cap)
{
    char tmp[PATH_MAX];
    uint32_t n = sizeof(tmp);
    if (_NSGetExecutablePath(tmp, &n) != 0)
        return NULL;
    if (!realpath(tmp, buf))
        return NULL;
    (void)cap; /* realpath는 PATH_MAX 버퍼를 요구 — 호출자는 PATH_MAX 사용 */
    return buf;
}
