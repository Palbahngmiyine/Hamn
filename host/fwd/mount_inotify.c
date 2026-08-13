#include "fwd/mount_inotify.h"

#include <CoreServices/CoreServices.h>
#include <dispatch/dispatch.h>

#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include "core/log.h"
#include "util/fs.h"
#include "util/proc.h"

#define MOUNT_INOTIFY_LEASE_TOKEN_SIZE 33
#define MOUNT_INOTIFY_MAX_ROOTS (PROFILE_MAX_MOUNTS + 1)
#define MOUNT_INOTIFY_RECENT_EVENTS 128
#define MOUNT_INOTIFY_HTTP_CAP (PATH_MAX * 6 + 512)
#define MOUNT_INOTIFY_READY_WAIT_ATTEMPTS 60

struct mount_inotify_root {
    char tag[32];
    char path[PATH_MAX];
};

struct mount_inotify_recent {
    dev_t device;
    ino_t inode;
    time_t seconds;
    long nanoseconds;
};

struct mount_inotify_watcher {
    struct profile profile;
    char lease[MOUNT_INOTIFY_LEASE_TOKEN_SIZE];
    struct mount_inotify_root roots[MOUNT_INOTIFY_MAX_ROOTS];
    size_t root_count;
    struct mount_inotify_recent recent[MOUNT_INOTIFY_RECENT_EVENTS];
    size_t recent_next;
};

static int path_is_within(const char *base, const char *path)
{
    size_t length = strlen(base);
    return length > 0 && strncmp(base, path, length) == 0 &&
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

static int append_root(struct mount_inotify_watcher *watcher,
                       const char *tag, const char *path)
{
    if (!watcher || !tag || !path || watcher->root_count >=
        MOUNT_INOTIFY_MAX_ROOTS || strlen(tag) >=
        sizeof(watcher->roots[0].tag) || strlen(path) >= PATH_MAX) {
        errno = EINVAL;
        return -1;
    }
    struct mount_inotify_root *root = &watcher->roots[watcher->root_count++];
    snprintf(root->tag, sizeof(root->tag), "%s", tag);
    snprintf(root->path, sizeof(root->path), "%s", path);
    return 0;
}

static int watcher_roots_build(struct mount_inotify_watcher *watcher)
{
    if (!watcher || !watcher->profile.mount_inotify) {
        errno = EINVAL;
        return -1;
    }
    watcher->root_count = 0;
    const char *home = getenv("HOME");
    char home_real[PATH_MAX];
    if (!home || canonical_owned_directory(home, home_real, 0) != 0)
        return -1;
    if (watcher->profile.mount_home && !watcher->profile.home_read_only &&
        append_root(watcher, "home", home_real) != 0)
        return -1;

    for (size_t index = 0; index < watcher->profile.mount_count; index++) {
        const struct profile_mount *mount = &watcher->profile.mounts[index];
        if (!mount->writable)
            continue;
        char source[PATH_MAX], tag[32];
        if (canonical_owned_directory(mount->location, source, 1) != 0 ||
            !path_is_within(home_real, source)) {
            errno = EINVAL;
            return -1;
        }
        int written = snprintf(tag, sizeof(tag), "mount%zu", index);
        if (written < 0 || written >= (int)sizeof(tag) ||
            append_root(watcher, tag, source) != 0)
            return -1;
    }
    if (watcher->root_count == 0) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

static int lease_token_valid(const char *token)
{
    if (!token || strlen(token) != MOUNT_INOTIFY_LEASE_TOKEN_SIZE - 1)
        return 0;
    for (size_t index = 0; token[index]; index++) {
        if (!((token[index] >= '0' && token[index] <= '9') ||
              (token[index] >= 'a' && token[index] <= 'f')))
            return 0;
    }
    return 1;
}

static void lease_token_generate(char token[MOUNT_INOTIFY_LEASE_TOKEN_SIZE])
{
    unsigned char random[16];
    static const char digits[] = "0123456789abcdef";
    arc4random_buf(random, sizeof(random));
    for (size_t index = 0; index < sizeof(random); index++) {
        token[index * 2] = digits[random[index] >> 4];
        token[index * 2 + 1] = digits[random[index] & 0x0f];
    }
    token[MOUNT_INOTIFY_LEASE_TOKEN_SIZE - 1] = '\0';
}

static int lease_path(const struct profile *profile, char *path, size_t cap)
{
    return profile && profile_path(profile, "mount-inotify.lease", path, cap);
}

static int ready_path(const struct profile *profile, char *path, size_t cap)
{
    return profile && profile_path(profile, "mount-inotify.ready", path, cap);
}

static int token_file_matches(const char *path, const char *token)
{
    if (!path || !lease_token_valid(token))
        return 0;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return 0;
    struct stat status;
    char actual[MOUNT_INOTIFY_LEASE_TOKEN_SIZE + 1] = {0};
    size_t expected = strlen(token) + 1;
    ssize_t received;
    do {
        received = read(fd, actual, expected);
    } while (received < 0 && errno == EINTR);
    char extra = '\0';
    ssize_t after;
    do {
        after = read(fd, &extra, 1);
    } while (after < 0 && errno == EINTR);
    int valid = fstat(fd, &status) == 0 && S_ISREG(status.st_mode) &&
        status.st_uid == geteuid() && (status.st_mode & 077) == 0 &&
        status.st_nlink == 1 && status.st_size == (off_t)expected &&
        received == (ssize_t)expected && after == 0 &&
        memcmp(actual, token, expected - 1) == 0 &&
        actual[expected - 1] == '\n';
    close(fd);
    return valid;
}

static int lease_matches(const struct profile *profile, const char *token)
{
    char path[PATH_MAX];
    return lease_path(profile, path, sizeof(path)) &&
        token_file_matches(path, token);
}

static int ready_matches(const struct profile *profile, const char *token)
{
    char path[PATH_MAX];
    return ready_path(profile, path, sizeof(path)) &&
        token_file_matches(path, token);
}

int mount_inotify_revoke(const struct profile *profile)
{
    char lease[PATH_MAX], ready[PATH_MAX];
    if (!lease_path(profile, lease, sizeof(lease)) ||
        !ready_path(profile, ready, sizeof(ready))) {
        errno = EINVAL;
        return -1;
    }
    int rc = 0, saved = 0;
    if (fs_unlink_if_exists(lease) != 0) {
        rc = -1;
        saved = errno;
    }
    if (fs_unlink_if_exists(ready) != 0) {
        if (rc == 0)
            saved = errno;
        rc = -1;
    }
    if (rc != 0)
        errno = saved;
    return rc;
}

static int token_file_write(const char *path, const char *token)
{
    char text[MOUNT_INOTIFY_LEASE_TOKEN_SIZE + 1];
    if (!path || !lease_token_valid(token)) {
        errno = EINVAL;
        return -1;
    }
    int length = snprintf(text, sizeof(text), "%s\n", token);
    return length == MOUNT_INOTIFY_LEASE_TOKEN_SIZE ?
        fs_write_file_atomic(path, text, (size_t)length, 0600) : -1;
}

static int lease_write(const struct profile *profile, const char *token)
{
    char path[PATH_MAX];
    if (!lease_path(profile, path, sizeof(path))) {
        errno = EINVAL;
        return -1;
    }
    return token_file_write(path, token);
}

static int ready_write(const struct profile *profile, const char *token)
{
    char path[PATH_MAX];
    if (!ready_path(profile, path, sizeof(path))) {
        errno = EINVAL;
        return -1;
    }
    return token_file_write(path, token);
}

static int socket_path_safe(const char *path)
{
    struct stat status;
    return lstat(path, &status) == 0 && S_ISSOCK(status.st_mode) &&
        status.st_uid == geteuid() && (status.st_mode & 077) == 0;
}

static int write_all(int fd, const char *data, size_t length)
{
    while (length > 0) {
        ssize_t written = send(fd, data, length, 0);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0)
            return -1;
        data += written;
        length -= (size_t)written;
    }
    return 0;
}

static int json_append_char(char *text, size_t cap, size_t *length, char value)
{
    if (*length + 1 >= cap)
        return -1;
    text[(*length)++] = value;
    text[*length] = '\0';
    return 0;
}

static int json_append_string(char *text, size_t cap, size_t *length,
                              const char *value)
{
    if (json_append_char(text, cap, length, '"') != 0)
        return -1;
    for (const unsigned char *cursor = (const unsigned char *)value; *cursor;
         cursor++) {
        if (*cursor == '"' || *cursor == '\\') {
            if (json_append_char(text, cap, length, '\\') != 0 ||
                json_append_char(text, cap, length, (char)*cursor) != 0)
                return -1;
        } else if (*cursor < 0x20) {
            int written;
            if (*length >= cap ||
                (written = snprintf(text + *length, cap - *length,
                                    "\\u%04x", *cursor)) != 6 ||
                (size_t)written >= cap - *length)
                return -1;
            *length += (size_t)written;
        } else if (json_append_char(text, cap, length, (char)*cursor) != 0) {
            return -1;
        }
    }
    return json_append_char(text, cap, length, '"');
}

static int send_agent_event(const struct profile *profile, const char *tag,
                            const char *relative, const struct stat *status)
{
    char socket_path[PATH_MAX];
    if (!profile_path(profile, "agent.sock", socket_path,
                      sizeof(socket_path)) || !socket_path_safe(socket_path)) {
        errno = ENOENT;
        return -1;
    }
    char body[MOUNT_INOTIFY_HTTP_CAP] = "{";
    size_t body_length = 1;
    if (json_append_string(body, sizeof(body), &body_length, "tag") != 0 ||
        json_append_char(body, sizeof(body), &body_length, ':') != 0 ||
        json_append_string(body, sizeof(body), &body_length, tag) != 0 ||
        json_append_char(body, sizeof(body), &body_length, ',') != 0 ||
        json_append_string(body, sizeof(body), &body_length, "path") != 0 ||
        json_append_char(body, sizeof(body), &body_length, ':') != 0 ||
        json_append_string(body, sizeof(body), &body_length, relative) != 0 ||
        json_append_char(body, sizeof(body), &body_length, ',') != 0 ||
        json_append_string(body, sizeof(body), &body_length, "mtimeSec") != 0 ||
        json_append_char(body, sizeof(body), &body_length, ':') != 0)
        return -1;
    int written = snprintf(body + body_length, sizeof(body) - body_length,
                           "%lld", (long long)status->st_mtimespec.tv_sec);
    if (written < 0 || (size_t)written >= sizeof(body) - body_length)
        return -1;
    body_length += (size_t)written;
    if (json_append_char(body, sizeof(body), &body_length, ',') != 0 ||
        json_append_string(body, sizeof(body), &body_length, "mtimeNsec") != 0 ||
        json_append_char(body, sizeof(body), &body_length, ':') != 0)
        return -1;
    written = snprintf(body + body_length, sizeof(body) - body_length, "%ld",
                       status->st_mtimespec.tv_nsec);
    if (written < 0 || (size_t)written >= sizeof(body) - body_length)
        return -1;
    body_length += (size_t)written;
    if (json_append_char(body, sizeof(body), &body_length, '}') != 0)
        return -1;

    char request[MOUNT_INOTIFY_HTTP_CAP + 256];
    written = snprintf(request, sizeof(request),
                       "POST /v1/mount-inotify HTTP/1.1\r\n"
                       "Host: hamn\r\nContent-Type: application/json\r\n"
                       "Content-Length: %zu\r\nConnection: close\r\n\r\n%s",
                       body_length, body);
    if (written < 0 || (size_t)written >= sizeof(request))
        return -1;

    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;
    if (fcntl(fd, F_SETFD, FD_CLOEXEC) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    struct timeval timeout = { .tv_sec = 1, .tv_usec = 0 };
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    struct sockaddr_un address;
    memset(&address, 0, sizeof(address));
    address.sun_family = AF_UNIX;
    if (strlen(socket_path) >= sizeof(address.sun_path)) {
        close(fd);
        errno = ENAMETOOLONG;
        return -1;
    }
    snprintf(address.sun_path, sizeof(address.sun_path), "%s", socket_path);
    if (connect(fd, (const struct sockaddr *)&address, sizeof(address)) != 0 ||
        write_all(fd, request, (size_t)written) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    char response[128];
    ssize_t received;
    do {
        received = recv(fd, response, sizeof(response) - 1, 0);
    } while (received < 0 && errno == EINTR);
    int saved = errno;
    close(fd);
    if (received <= 0) {
        errno = received == 0 ? EPROTO : saved;
        return -1;
    }
    response[received] = '\0';
    if (strncmp(response, "HTTP/1.1 204 ", 13) != 0 &&
        strncmp(response, "HTTP/1.0 204 ", 13) != 0) {
        errno = EPROTO;
        return -1;
    }
    return 0;
}

static const struct mount_inotify_root *find_root(
    const struct mount_inotify_watcher *watcher, const char *path)
{
    const struct mount_inotify_root *best = NULL;
    for (size_t index = 0; index < watcher->root_count; index++) {
        const struct mount_inotify_root *candidate = &watcher->roots[index];
        if (path_is_within(candidate->path, path) &&
            (!best || strlen(candidate->path) > strlen(best->path)))
            best = candidate;
    }
    return best;
}

static int recently_sent(struct mount_inotify_watcher *watcher,
                         const struct stat *status)
{
    for (size_t index = 0; index < MOUNT_INOTIFY_RECENT_EVENTS; index++) {
        const struct mount_inotify_recent *recent = &watcher->recent[index];
        if (recent->device == status->st_dev && recent->inode == status->st_ino &&
            recent->seconds == status->st_mtimespec.tv_sec &&
            recent->nanoseconds == status->st_mtimespec.tv_nsec)
            return 1;
    }
    struct mount_inotify_recent *recent =
        &watcher->recent[watcher->recent_next++ % MOUNT_INOTIFY_RECENT_EVENTS];
    recent->device = status->st_dev;
    recent->inode = status->st_ino;
    recent->seconds = status->st_mtimespec.tv_sec;
    recent->nanoseconds = status->st_mtimespec.tv_nsec;
    return 0;
}

static void handle_event(struct mount_inotify_watcher *watcher,
                         const char *event_path, FSEventStreamEventFlags flags)
{
    if ((flags & (kFSEventStreamEventFlagMustScanSubDirs |
                  kFSEventStreamEventFlagUserDropped |
                  kFSEventStreamEventFlagKernelDropped |
                  kFSEventStreamEventFlagRootChanged)) ||
        !(flags & kFSEventStreamEventFlagItemIsFile) ||
        !(flags & kFSEventStreamEventFlagItemModified))
        return;
    struct stat link_status, status;
    char path[PATH_MAX];
    if (lstat(event_path, &link_status) != 0 || !S_ISREG(link_status.st_mode) ||
        !realpath(event_path, path) || stat(path, &status) != 0 ||
        !S_ISREG(status.st_mode))
        return;
    const struct mount_inotify_root *root = find_root(watcher, path);
    if (!root || strcmp(path, root->path) == 0 || recently_sent(watcher, &status))
        return;
    size_t root_length = strlen(root->path);
    const char *relative = path + root_length;
    if (*relative == '/')
        relative++;
    if (!relative[0] || send_agent_event(&watcher->profile, root->tag, relative,
                                         &status) != 0)
        logerr("mountInotify could not relay %s: %s", path, strerror(errno));
}

static void fsevent_callback(ConstFSEventStreamRef stream, void *context,
                             size_t count, void *paths,
                             const FSEventStreamEventFlags flags[],
                             const FSEventStreamEventId ids[])
{
    (void)stream;
    (void)ids;
    struct mount_inotify_watcher *watcher = context;
    CFArrayRef values = paths;
    for (size_t index = 0; index < count; index++) {
        CFStringRef value = CFArrayGetValueAtIndex(values, (CFIndex)index);
        char path[PATH_MAX];
        if (!value || !CFStringGetCString(value, path, sizeof(path),
                                          kCFStringEncodingUTF8))
            continue;
        handle_event(watcher, path, flags[index]);
    }
}

static void dispatch_noop(void *context)
{
    (void)context;
}

static int mount_inotify_watch(struct mount_inotify_watcher *watcher)
{
    if (watcher_roots_build(watcher) != 0)
        return -1;
    CFMutableArrayRef paths = CFArrayCreateMutable(NULL, (CFIndex)watcher->root_count,
                                                    &kCFTypeArrayCallBacks);
    if (!paths)
        return -1;
    int rc = -1;
    for (size_t index = 0; index < watcher->root_count; index++) {
        CFStringRef path = CFStringCreateWithCString(NULL, watcher->roots[index].path,
                                                      kCFStringEncodingUTF8);
        if (!path)
            goto out;
        CFArrayAppendValue(paths, path);
        CFRelease(path);
    }
    FSEventStreamContext context = { .version = 0, .info = watcher };
    FSEventStreamRef stream = FSEventStreamCreate(
        NULL, fsevent_callback, &context, paths,
        kFSEventStreamEventIdSinceNow, 0.1,
        kFSEventStreamCreateFlagUseCFTypes |
        kFSEventStreamCreateFlagFileEvents |
        kFSEventStreamCreateFlagNoDefer |
        kFSEventStreamCreateFlagWatchRoot);
    if (!stream)
        goto out;
    dispatch_queue_t queue = dispatch_queue_create("dev.hamn.mount-inotify",
                                                    DISPATCH_QUEUE_SERIAL);
    if (!queue) {
        FSEventStreamInvalidate(stream);
        FSEventStreamRelease(stream);
        goto out;
    }
    FSEventStreamSetDispatchQueue(stream, queue);
    if (!FSEventStreamStart(stream)) {
        FSEventStreamInvalidate(stream);
        FSEventStreamRelease(stream);
        dispatch_release(queue);
        goto out;
    }
    if (!lease_matches(&watcher->profile, watcher->lease) ||
        ready_write(&watcher->profile, watcher->lease) != 0) {
        FSEventStreamStop(stream);
        FSEventStreamInvalidate(stream);
        dispatch_sync_f(queue, NULL, dispatch_noop);
        FSEventStreamRelease(stream);
        dispatch_release(queue);
        goto out;
    }
    rc = 0;
    while (lease_matches(&watcher->profile, watcher->lease)) {
        struct timespec delay = { .tv_sec = 1, .tv_nsec = 0 };
        while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {}
    }
    FSEventStreamStop(stream);
    FSEventStreamInvalidate(stream);
    dispatch_sync_f(queue, NULL, dispatch_noop);
    FSEventStreamRelease(stream);
    dispatch_release(queue);
out:
    CFRelease(paths);
    return rc;
}

struct watcher_options {
    const char *profile;
    const char *lease;
};

static int watcher_options_parse(int argc, char **argv,
                                 struct watcher_options *options)
{
    memset(options, 0, sizeof(*options));
    static const struct option long_options[] = {
        { "profile", required_argument, NULL, 'p' },
        { "lease", required_argument, NULL, 'l' },
        { 0 },
    };
    optind = 1;
    optreset = 1;
    int option;
    while ((option = getopt_long(argc, argv, "p:l:", long_options, NULL)) != -1) {
        if ((option == 'p' && !options->profile) ||
            (option == 'l' && !options->lease)) {
            if (option == 'p')
                options->profile = optarg;
            else
                options->lease = optarg;
            continue;
        }
        return -1;
    }
    return optind == argc && profile_name_valid(options->profile) &&
        lease_token_valid(options->lease) ? 0 : -1;
}

int cmd_mount_inotify_watch(int argc, char **argv)
{
    struct watcher_options options;
    if (watcher_options_parse(argc, argv, &options) != 0)
        return 2;
    struct mount_inotify_watcher watcher;
    memset(&watcher, 0, sizeof(watcher));
    if (profile_load(&watcher.profile, options.profile) != 0 ||
        !watcher.profile.mount_inotify) {
        logerr("cannot load mountInotify profile %s", options.profile);
        return 1;
    }
    snprintf(watcher.lease, sizeof(watcher.lease), "%s", options.lease);
    return mount_inotify_watch(&watcher) == 0 ? 0 : 1;
}

int mount_inotify_start(const struct profile *profile)
{
    if (!profile) {
        errno = EINVAL;
        return -1;
    }
    if (mount_inotify_revoke(profile) != 0)
        return -1;
    if (!profile->mount_inotify)
        return 0;
    struct mount_inotify_watcher validation;
    memset(&validation, 0, sizeof(validation));
    validation.profile = *profile;
    if (watcher_roots_build(&validation) != 0)
        return -1;
    char token[MOUNT_INOTIFY_LEASE_TOKEN_SIZE], self[PATH_MAX];
    char logs[PATH_MAX], logfile[PATH_MAX];
    lease_token_generate(token);
    if (lease_write(profile, token) != 0 || !proc_self_path(self, sizeof(self)) ||
        !profile_path(profile, "logs", logs, sizeof(logs)) ||
        fs_mkdirs(logs, 0755) != 0 ||
        !profile_path(profile, "logs/mount-inotify.log", logfile,
                      sizeof(logfile))) {
        (void)mount_inotify_revoke(profile);
        return -1;
    }
    const char *argv[] = {
        self, "mount-inotify-watch", "--profile", profile->name,
        "--lease", token, NULL,
    };
    if (proc_spawn_daemon(argv, logfile) < 0) {
        (void)mount_inotify_revoke(profile);
        return -1;
    }
    for (unsigned attempt = 0; attempt < MOUNT_INOTIFY_READY_WAIT_ATTEMPTS;
         attempt++) {
        if (ready_matches(profile, token))
            return 0;
        if (!lease_matches(profile, token))
            break;
        usleep(50 * 1000);
    }
    (void)mount_inotify_revoke(profile);
    errno = ETIMEDOUT;
    return -1;
}
