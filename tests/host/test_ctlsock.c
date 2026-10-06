/* The control socket's server and its status client.
 *
 * A client connects and then sends, so a server can accept a connection whose
 * request has not arrived yet. Supervisors through Hamn 0.2.1 closed such a
 * connection, and a healthy VM was then reported as "unknown". The tests
 * below place a request after the accept with explicit ordering: the server
 * accepts connections in the order they were made, so its reply to a later
 * connection proves that it has accepted every earlier one. To place events
 * before an accept instead, a test holds the server inside its stop callback.
 * No test orders events by sleeping. FAILURE_DEADLINE_SEC only turns a hang
 * into a failure, and the one timeout that is waited for is the client's own,
 * in the test of a reply that never comes. */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
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

#define FAILURE_DEADLINE_SEC 10

static const char STATUS_REQUEST[] = "{\"cmd\":\"status\"}\n";

static int connect_client(const char *path)
{
    int fd = connect_unix_socket(path);
    if (fd < 0)
        fail("connect to the control socket");
    struct timeval deadline = { .tv_sec = FAILURE_DEADLINE_SEC };
    require(setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &deadline,
                       sizeof(deadline)) == 0,
            "client connection must take a receive deadline");
    return fd;
}

static void send_text(int fd, const char *text)
{
    require(write_all(fd, text, strlen(text)) == 0,
            "the server must keep a connection open until it has answered");
}

static void expect_status_reply(int fd, const char *message)
{
    char reply[256];
    ssize_t n = read(fd, reply, sizeof(reply) - 1);
    require(n > 0, message);
    reply[n] = '\0';
    require(strstr(reply, "\"state\":\"running\"") != NULL, message);
}

static void wait_until_accepted(const char *path)
{
    char response[256];
    require(ctlsock_query_status(path, response, sizeof(response),
                                 FAILURE_DEADLINE_SEC * 1000) ==
                CTLSOCK_QUERY_OK,
            "a status query must be answered while other connections wait");
}

/* The serving child's stop callback reports the request and then keeps the
 * server's main queue busy until the test releases it. */
static int stop_report_fd = -1;
static int stop_release_fd = -1;
static int server_stops_fd = -1;
static int server_release_fd = -1;

static void report_stop_and_wait(void)
{
    char byte = 's';
    if (write_all(stop_report_fd, &byte, 1) != 0 ||
        read_all(stop_release_fd, &byte, 1) != 0)
        _exit(4);
}

static void expect_stop_reported(const char *message)
{
    struct pollfd reported = { .fd = server_stops_fd, .events = POLLIN };
    char byte = 0;
    require(poll(&reported, 1, FAILURE_DEADLINE_SEC * 1000) == 1 &&
                read(server_stops_fd, &byte, 1) == 1 && byte == 's',
            message);
}

static void release_server(void)
{
    require(write_all(server_release_fd, "r", 1) == 0,
            "held server must be releasable");
}

/* Leaves the server held in its stop callback: it accepts nothing more until
 * release_server(). */
static void hold_server(const char *path)
{
    int fd = connect_client(path);
    send_text(fd, "{\"cmd\":\"stop\"}\n");
    char reply[64];
    ssize_t n = read(fd, reply, sizeof(reply) - 1);
    require(n > 0, "a stop request must be acknowledged before it is "
                   "carried out");
    reply[n] = '\0';
    require(strcmp(reply, "{\"ok\":true}\n") == 0,
            "a stop request must be acknowledged with ok");
    require(close(fd) == 0, "stop client must close");
    expect_stop_reported("an acknowledged stop request must be carried out");
}

static void test_stop_is_acknowledged_then_carried_out(const char *path)
{
    hold_server(path);
    release_server();
}

static void test_request_of_a_peer_that_already_closed_is_carried_out(
    const char *path)
{
    hold_server(path);
    int fd = connect_client(path);
    send_text(fd, "{\"cmd\":\"stop\"}\n");
    require(close(fd) == 0, "sender must close before it is accepted");
    release_server();
    expect_stop_reported("a request must be carried out although its sender "
                         "closed before the server accepted it");
    release_server();
    char response[256];
    require(ctlsock_query_status(path, response, sizeof(response),
                                 FAILURE_DEADLINE_SEC * 1000) ==
                CTLSOCK_QUERY_OK,
            "the server must outlive a request whose sender has closed");
}

static void test_request_sent_after_accept_is_answered(const char *path)
{
    int fd = connect_client(path);
    wait_until_accepted(path);
    send_text(fd, STATUS_REQUEST);
    expect_status_reply(fd, "a request that arrives after its connection "
                            "was accepted must be answered");
    require(close(fd) == 0, "late client must close");
}

/* The barrier lets the server read the first piece on its own; the reply
 * must be the same when it reads both pieces together. */
static void test_request_sent_in_pieces_is_answered(const char *path)
{
    int fd = connect_client(path);
    send_text(fd, "{\"cmd\":\"st");
    wait_until_accepted(path);
    send_text(fd, "atus\"}\n");
    expect_status_reply(fd, "a request line that arrives in pieces must be "
                            "answered as one request");
    require(close(fd) == 0, "piecewise client must close");
}

static void test_request_without_newline_is_answered_at_end_of_input(
    const char *path)
{
    int fd = connect_client(path);
    send_text(fd, "{\"cmd\":\"status\"}");
    require(shutdown(fd, SHUT_WR) == 0, "client must end its input");
    expect_status_reply(fd, "a request ended by end of input must be "
                            "answered");
    require(close(fd) == 0, "half-closed client must close");
}

static void test_longest_waiting_connection_gives_way(const char *path)
{
    int waiting[CTLSOCK_WAITING_MAX];
    for (int i = 0; i < CTLSOCK_WAITING_MAX; i++) {
        waiting[i] = connect_client(path);
        /* The barrier waits beside the others, so the last connection that
         * fits is left to the query below. */
        if (i < CTLSOCK_WAITING_MAX - 1)
            wait_until_accepted(path);
    }
    wait_until_accepted(path);

    char byte;
    require(read(waiting[0], &byte, 1) == 0,
            "a full server must close the connection that waited longest");
    require(close(waiting[0]) == 0, "closed-out client must close");
    for (int i = 1; i < CTLSOCK_WAITING_MAX; i++) {
        send_text(waiting[i], STATUS_REQUEST);
        expect_status_reply(waiting[i], "a full server must keep every "
                                        "connection but the oldest");
        require(close(waiting[i]) == 0, "waiting client must close");
    }
}

/* A slot that stayed taken would make each of these connections push an older
 * one out, the kept connection first. */
static void test_peer_that_closes_while_waiting_frees_its_place(
    const char *path)
{
    int kept = connect_client(path);
    wait_until_accepted(path);
    for (int i = 0; i < 2 * CTLSOCK_WAITING_MAX; i++) {
        int fd = connect_client(path);
        wait_until_accepted(path);
        require(close(fd) == 0, "silent client must close");
    }
    send_text(kept, STATUS_REQUEST);
    expect_status_reply(kept, "connections closed while they waited must "
                              "not use up the places of waiting ones");
    require(close(kept) == 0, "kept client must close");
}

static void test_peer_that_closed_before_accept_frees_its_place(
    const char *path)
{
    enum { PER_HOLD = 4 }; /* below the server's listen backlog */
    int kept = connect_client(path);
    wait_until_accepted(path);
    for (int i = 0; i < 2 * CTLSOCK_WAITING_MAX; i += PER_HOLD) {
        hold_server(path);
        for (int j = 0; j < PER_HOLD; j++)
            require(close(connect_client(path)) == 0,
                    "silent client must close before it is accepted");
        release_server();
        wait_until_accepted(path);
    }
    send_text(kept, STATUS_REQUEST);
    expect_status_reply(kept, "connections closed before they were accepted "
                              "must not use up the places of waiting ones");
    require(close(kept) == 0, "kept client must close");
}

static void test_serve_and_query(const char *path)
{
    int ready[2], stops[2], release[2];
    if (pipe(ready) != 0 || pipe(stops) != 0 || pipe(release) != 0)
        fail("pipe");
    pid_t child = fork();
    if (child < 0)
        fail("fork");
    if (child == 0) {
        close(ready[0]);
        close(stops[0]);
        close(release[1]);
        stop_report_fd = stops[1];
        stop_release_fd = release[0];
        /* The server must not depend on its process ignoring SIGPIPE. */
        if (signal(SIGPIPE, SIG_DFL) == SIG_ERR)
            _exit(5);
        struct serve_result result = { 0 };
        result.rc = ctlsock_serve(path, NULL, 11, 22, report_stop_and_wait,
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
    close(stops[1]);
    close(release[0]);
    server_stops_fd = stops[0];
    server_release_fd = release[1];
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
    require(ctlsock_query_status(path, response, sizeof(response), 1000) ==
                CTLSOCK_QUERY_OK,
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

    test_request_sent_after_accept_is_answered(path);
    test_request_sent_in_pieces_is_answered(path);
    test_request_without_newline_is_answered_at_end_of_input(path);
    test_stop_is_acknowledged_then_carried_out(path);
    test_request_of_a_peer_that_already_closed_is_carried_out(path);
    test_longest_waiting_connection_gives_way(path);
    test_peer_that_closes_while_waiting_frees_its_place(path);
    test_peer_that_closed_before_accept_frees_its_place(path);

    require(kill(child, SIGKILL) == 0, "serve child must be killable");
    require(close(server_stops_fd) == 0 && close(server_release_fd) == 0,
            "serve child pipes must close");
    int status;
    require(waitpid(child, &status, 0) == child && WIFSIGNALED(status) &&
                WTERMSIG(status) == SIGKILL,
            "serve child must exit by SIGKILL");
    require(lstat(path, &sb) == 0 && S_ISSOCK(sb.st_mode),
            "filesystem socket must remain after abrupt owner death");
    require(ctlsock_query_status(path, response, sizeof(response), 1000) ==
                CTLSOCK_QUERY_UNAVAILABLE,
            "a socket path nobody serves must be unavailable");
    require(ctlsock_unlink_owned(path, result.dev, result.ino) == 0,
            "captured owner identity must remove its stale socket");
    require(lstat(path, &sb) != 0 && errno == ENOENT,
            "owned stale socket must be absent after cleanup");
    require(ctlsock_query_status(path, response, sizeof(response), 1000) ==
                CTLSOCK_QUERY_UNAVAILABLE,
            "a missing socket path must be unavailable");
}

/* What a scripted supervisor does with one accepted connection. */
enum fake_step {
    /* Close before the request was sent, as a supervisor through Hamn 0.2.1
     * does when it reads first: the client's send then fails. */
    FAKE_CLOSE_UNREAD,
    /* Close after the request arrived, without replying. */
    FAKE_CLOSE_READ,
    FAKE_REPLY,
    /* Keep the connection open and never reply. */
    FAKE_SILENT,
};

static const char FAKE_REPLY_TEXT[] =
    "{\"state\":\"running\",\"pid\":4242,\"start_sec\":7,\"start_usec\":9}\n";

static int fake_closed_fd = -1;
static int fake_closes_to_await;
static int status_attempts;

/* Counts the client's connections and holds each send back until the
 * scripted supervisor has closed the connection it is about to use. */
static void before_send(void)
{
    status_attempts++;
    if (fake_closes_to_await == 0)
        return;
    fake_closes_to_await--;
    char byte;
    require(read_all(fake_closed_fd, &byte, 1) == 0,
            "scripted supervisor must report each early close");
}

static void fake_supervise(int listener, const enum fake_step *steps,
                           size_t count, int closed_fd)
{
    for (size_t i = 0;; i++) {
        int client = accept(listener, NULL, NULL);
        if (client < 0)
            _exit(70);
        /* A connection beyond the script is answered, so a query that makes
         * one too many does not end as its test expects. */
        enum fake_step step = i < count ? steps[i] : FAKE_REPLY;
        if (step == FAKE_CLOSE_UNREAD) {
            if (close(client) != 0 || write_all(closed_fd, "c", 1) != 0)
                _exit(71);
            continue;
        }
        char request[64];
        if (read_all(client, request, sizeof(STATUS_REQUEST) - 1) != 0 ||
            memcmp(request, STATUS_REQUEST, sizeof(STATUS_REQUEST) - 1) != 0)
            _exit(72);
        if (step == FAKE_SILENT)
            continue;
        if (step == FAKE_REPLY &&
            write_all(client, FAKE_REPLY_TEXT,
                      sizeof(FAKE_REPLY_TEXT) - 1) != 0)
            _exit(73);
        if (close(client) != 0)
            _exit(74);
    }
}

/* Runs one status query against a supervisor that treats the client's
 * successive connections as steps say, and returns the query's result. */
static int query_scripted_supervisor(const char *path,
                                     const enum fake_step *steps,
                                     size_t count, int timeout_ms,
                                     char *response, size_t response_size)
{
    int listener = bind_unix_socket(path);
    int closed[2];
    if (listener < 0 || listen(listener, 8) != 0 || pipe(closed) != 0)
        fail("scripted supervisor fixture");
    pid_t child = fork();
    if (child < 0)
        fail("fork");
    if (child == 0) {
        close(closed[0]);
        fake_supervise(listener, steps, count, closed[1]);
    }
    close(closed[1]);
    close(listener);

    fake_closed_fd = closed[0];
    fake_closes_to_await = 0;
    for (size_t i = 0; i < count; i++)
        fake_closes_to_await += steps[i] == FAKE_CLOSE_UNREAD;
    status_attempts = 0;
    ctlsock_test_before_send(before_send);
    int result = ctlsock_query_status(path, response, response_size,
                                      timeout_ms);
    ctlsock_test_before_send(NULL);

    int status;
    require(kill(child, SIGKILL) == 0 && waitpid(child, &status, 0) == child &&
                WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL,
            "scripted supervisor must run until it is killed");
    require(close(closed[0]) == 0 && unlink(path) == 0,
            "scripted supervisor fixture must be removable");
    return result;
}

static void test_status_is_sent_again_when_closed_unanswered(const char *path)
{
    static const enum fake_step kinds[] = { FAKE_CLOSE_UNREAD,
                                            FAKE_CLOSE_READ };
    for (size_t kind = 0; kind < sizeof(kinds) / sizeof(kinds[0]); kind++) {
        enum fake_step steps[CTLSOCK_STATUS_ATTEMPTS];
        for (int i = 0; i < CTLSOCK_STATUS_ATTEMPTS - 1; i++)
            steps[i] = kinds[kind];
        steps[CTLSOCK_STATUS_ATTEMPTS - 1] = FAKE_REPLY;

        char response[256];
        require(query_scripted_supervisor(path, steps,
                                          CTLSOCK_STATUS_ATTEMPTS,
                                          FAILURE_DEADLINE_SEC * 1000,
                                          response, sizeof(response)) ==
                    CTLSOCK_QUERY_OK,
                "status must be sent again after an unanswered close");
        require(strcmp(response, FAKE_REPLY_TEXT) == 0,
                "the reply of the answered connection must be returned");
        require(status_attempts == CTLSOCK_STATUS_ATTEMPTS,
                "each unanswered close must cost exactly one connection");
    }
}

static void test_status_attempts_are_bounded(const char *path)
{
    static const enum fake_step kinds[] = { FAKE_CLOSE_UNREAD,
                                            FAKE_CLOSE_READ };
    for (size_t kind = 0; kind < sizeof(kinds) / sizeof(kinds[0]); kind++) {
        enum fake_step steps[CTLSOCK_STATUS_ATTEMPTS];
        for (int i = 0; i < CTLSOCK_STATUS_ATTEMPTS; i++)
            steps[i] = kinds[kind];

        char response[256];
        require(query_scripted_supervisor(path, steps,
                                          CTLSOCK_STATUS_ATTEMPTS,
                                          FAILURE_DEADLINE_SEC * 1000,
                                          response, sizeof(response)) ==
                    CTLSOCK_QUERY_UNCERTAIN,
                "a server that never answers must stay uncertain");
        require(status_attempts == CTLSOCK_STATUS_ATTEMPTS,
                "status must not be sent on more connections than allowed");
    }
}

/* The supervisor would answer a second connection, so a query that is still
 * uncertain did not send again. */
static void test_status_is_not_sent_again_after_a_timeout(const char *path)
{
    static const enum fake_step steps[] = { FAKE_SILENT };
    char response[256];
    require(query_scripted_supervisor(path, steps, 1, 50, response,
                                      sizeof(response)) ==
                CTLSOCK_QUERY_UNCERTAIN,
            "a reply that does not come in time must be uncertain");
    require(status_attempts == 1,
            "a timed-out status must not be sent again");
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

    /* A send to a connection the server closed must fail a check, not end
     * the test with SIGPIPE. */
    if (signal(SIGPIPE, SIG_IGN) == SIG_ERR)
        fail("signal");

    test_serve_and_query(path);
    test_status_is_sent_again_when_closed_unanswered(path);
    test_status_attempts_are_bounded(path);
    test_status_is_not_sent_again_after_a_timeout(path);
    test_nonblocking_failure_is_cleaned(path);
    test_existing_regular_file_is_preserved(path);
    test_existing_socket_is_preserved(path);
    test_replaced_socket_is_preserved(path, owned_path);
    require(rmdir(directory) == 0, "fixture directory must be empty");
    puts("ctlsock tests: ok");
    return 0;
}
