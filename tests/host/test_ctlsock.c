#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#include <dispatch/dispatch.h>

#include "vmrun/ctlsock.h"
#include "vz/vz_shim.h"

struct serve_result {
    int rc;
    int saved_errno;
    dev_t dev;
    ino_t ino;
};

enum vz_state vz_vm_state(vz_vm *vm)
{
    (void)vm;
    return VZ_ST_RUNNING;
}

static void fail(const char *message)
{
    perror(message);
    exit(1);
}

static void require(int condition, const char *message)
{
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", message);
        exit(1);
    }
}

static int write_all(int fd, const void *data, size_t length)
{
    const char *bytes = data;
    size_t offset = 0;
    while (offset < length) {
        ssize_t written = write(fd, bytes + offset, length - offset);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0)
            return -1;
        offset += (size_t)written;
    }
    return 0;
}

static int read_all(int fd, void *data, size_t length)
{
    char *bytes = data;
    size_t offset = 0;
    while (offset < length) {
        ssize_t received = read(fd, bytes + offset, length - offset);
        if (received < 0 && errno == EINTR)
            continue;
        if (received <= 0)
            return -1;
        offset += (size_t)received;
    }
    return 0;
}

static int bind_unix_socket(const char *path)
{
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    if (strlen(path) >= sizeof(sa.sun_path)) {
        close(fd);
        errno = ENAMETOOLONG;
        return -1;
    }
    strcpy(sa.sun_path, path);
    if (bind(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    return fd;
}

static int connect_unix_socket(const char *path)
{
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    if (strlen(path) >= sizeof(sa.sun_path)) {
        close(fd);
        errno = ENAMETOOLONG;
        return -1;
    }
    strcpy(sa.sun_path, path);
    if (connect(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    return fd;
}

static void test_serve_and_query(const char *path)
{
    int ready[2];
    if (pipe(ready) != 0)
        fail("pipe");
    pid_t child = fork();
    if (child < 0)
        fail("fork");
    if (child == 0) {
        close(ready[0]);
        struct serve_result result = { 0 };
        result.rc = ctlsock_serve(path, NULL, 11, 22, NULL,
                                  &result.dev, &result.ino);
        result.saved_errno = errno;
        if (write_all(ready[1], &result, sizeof(result)) != 0)
            _exit(2);
        close(ready[1]);
        if (result.rc != 0)
            _exit(3);
        dispatch_main();
    }

    close(ready[1]);
    struct serve_result result;
    require(read_all(ready[0], &result, sizeof(result)) == 0,
            "serve child must report readiness");
    close(ready[0]);
    if (result.rc != 0) {
        errno = result.saved_errno;
        fail("ctlsock_serve");
    }

    struct stat sb;
    require(lstat(path, &sb) == 0 && S_ISSOCK(sb.st_mode),
            "serve must publish a filesystem socket");
    require((sb.st_mode & 0777) == 0600,
            "control socket mode must be 0600");
    require(sb.st_uid == geteuid() && sb.st_nlink == 1,
            "control socket must be a private single-link owner path");
    require(sb.st_dev == result.dev && sb.st_ino == result.ino,
            "serve must return the published path identity");

    char response[256];
    require(ctlsock_query(path, "{\"cmd\":\"status\"}", response,
                          sizeof(response), 1000) == CTLSOCK_QUERY_OK,
            "status query must reach the serving control socket");
    char expected_pid[64];
    int n = snprintf(expected_pid, sizeof(expected_pid), "\"pid\":%d",
                     child);
    require(n > 0 && n < (int)sizeof(expected_pid),
            "child pid must format");
    require(strstr(response, "\"state\":\"running\"") != NULL &&
                strstr(response, expected_pid) != NULL &&
                strstr(response, "\"start_sec\":11") != NULL &&
                strstr(response, "\"start_usec\":22") != NULL,
            "status response must contain runtime identity");

    require(kill(child, SIGKILL) == 0, "serve child must be killable");
    int status;
    require(waitpid(child, &status, 0) == child && WIFSIGNALED(status) &&
                WTERMSIG(status) == SIGKILL,
            "serve child must exit by SIGKILL");
    require(lstat(path, &sb) == 0 && S_ISSOCK(sb.st_mode),
            "filesystem socket must remain after abrupt owner death");
    require(ctlsock_unlink_owned(path, result.dev, result.ino) == 0,
            "captured owner identity must remove its stale socket");
    require(lstat(path, &sb) != 0 && errno == ENOENT,
            "owned stale socket must be absent after cleanup");
}

static void test_existing_regular_file_is_preserved(const char *path)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0)
        fail("create foreign regular path");
    require(write_all(fd, "keep\n", 5) == 0 && close(fd) == 0,
            "foreign regular path must be written");

    dev_t dev = 99;
    ino_t ino = 99;
    require(ctlsock_serve(path, NULL, 0, 0, NULL, &dev, &ino) != 0,
            "serve must fail when the path already exists");
    require(dev == 0 && ino == 0,
            "failed serve must not claim a path identity");

    char contents[6] = { 0 };
    fd = open(path, O_RDONLY);
    require(fd >= 0 && read(fd, contents, 5) == 5 && close(fd) == 0 &&
                strcmp(contents, "keep\n") == 0,
            "failed serve must preserve an existing foreign file");
    require(unlink(path) == 0, "foreign regular fixture must be removable");
}

static void test_nonblocking_failure_is_cleaned(const char *path)
{
    dev_t dev = 99;
    ino_t ino = 99;
    ctlsock_test_fail_nonblocking_once(EIO);
    errno = 0;
    require(ctlsock_serve(path, NULL, 0, 0, NULL, &dev, &ino) != 0 &&
                errno == EIO,
            "nonblocking failure must fail socket publication");
    require(dev == 0 && ino == 0,
            "failed nonblocking setup must not publish path identity");
    struct stat sb;
    require(lstat(path, &sb) != 0 && errno == ENOENT,
            "failed nonblocking setup must remove its socket path");
}

static void test_existing_socket_is_preserved(const char *path)
{
    int foreign_fd = bind_unix_socket(path);
    if (foreign_fd < 0 || listen(foreign_fd, 2) != 0)
        fail("listen on existing foreign socket");
    struct stat before, after;
    require(lstat(path, &before) == 0 && S_ISSOCK(before.st_mode),
            "existing foreign path must be a socket");

    dev_t dev = 99;
    ino_t ino = 99;
    require(ctlsock_serve(path, NULL, 0, 0, NULL, &dev, &ino) != 0,
            "serve must fail when a live socket already exists");
    require(dev == 0 && ino == 0,
            "failed serve must not claim the existing socket identity");
    require(lstat(path, &after) == 0 && after.st_dev == before.st_dev &&
                after.st_ino == before.st_ino,
            "failed serve must preserve the existing socket path");
    int client_fd = connect_unix_socket(path);
    require(client_fd >= 0,
            "failed serve must preserve existing socket connectability");
    require(close(client_fd) == 0 && close(foreign_fd) == 0,
            "existing socket descriptors must close");
    require(unlink(path) == 0, "existing socket path must be removable");
}

static void test_replaced_socket_is_preserved(const char *path,
                                              const char *owned_path)
{
    dev_t owned_dev;
    ino_t owned_ino;
    require(ctlsock_serve(path, NULL, 0, 0, NULL, &owned_dev,
                          &owned_ino) == 0,
            "owner socket fixture must serve");
    require(rename(path, owned_path) == 0,
            "owner socket node must remain live under another path");

    int foreign_fd = bind_unix_socket(path);
    if (foreign_fd < 0 || listen(foreign_fd, 2) != 0)
        fail("listen on foreign replacement socket");
    struct stat before, after;
    require(lstat(path, &before) == 0 && S_ISSOCK(before.st_mode),
            "foreign replacement must be a filesystem socket");
    require(before.st_dev != owned_dev || before.st_ino != owned_ino,
            "foreign replacement must have a distinct identity");

    errno = 0;
    require(ctlsock_unlink_owned(path, owned_dev, owned_ino) != 0 &&
                errno == ESTALE,
            "cleanup must reject a same-type foreign replacement");
    require(lstat(path, &after) == 0 && after.st_dev == before.st_dev &&
                after.st_ino == before.st_ino,
            "cleanup must not remove or replace the foreign socket path");
    int client_fd = connect_unix_socket(path);
    require(client_fd >= 0,
            "cleanup must preserve foreign socket connectability");

    require(close(client_fd) == 0 && close(foreign_fd) == 0,
            "foreign socket descriptors must close");
    require(unlink(path) == 0, "foreign socket path must be removable");
    require(unlink(owned_path) == 0,
            "renamed owner socket path must be removable");
}

int main(void)
{
    char directory[] = "/tmp/hamn-ctlsock.XXXXXX";
    if (!mkdtemp(directory))
        fail("mkdtemp");
    char path[sizeof(((struct sockaddr_un *)0)->sun_path)];
    char owned_path[sizeof(((struct sockaddr_un *)0)->sun_path)];
    int path_length = snprintf(path, sizeof(path), "%s/control.sock",
                               directory);
    int owned_length = snprintf(owned_path, sizeof(owned_path),
                                "%s/owned.sock", directory);
    require(path_length > 0 && path_length < (int)sizeof(path) &&
                owned_length > 0 && owned_length < (int)sizeof(owned_path),
            "fixture paths must fit sockaddr_un");

    test_serve_and_query(path);
    test_nonblocking_failure_is_cleaned(path);
    test_existing_regular_file_is_preserved(path);
    test_existing_socket_is_preserved(path);
    test_replaced_socket_is_preserved(path, owned_path);
    require(rmdir(directory) == 0, "fixture directory must be empty");
    puts("ctlsock tests: ok");
    return 0;
}
