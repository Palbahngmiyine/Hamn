//! Owned-worker completion must not depend on background stderr writers.
use super::*;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::net::UnixListener;

/// Fixture workers: one C program, compiled per test with `-D<MODE>` into a
/// private directory. Each drains the request on stdin like the real worker;
/// sockets it uses are named relative to its own directory.
const WORKER_SOURCE: &str = r#"
#include <fcntl.h>
#include <libgen.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

/* Each mode uses only some of these helpers. */
#define HELPER static __attribute__((unused))

static struct sockaddr_un gate;

HELPER void put(int fd, const char *text) {
    size_t length = strlen(text);
    while (length) {
        ssize_t written = write(fd, text, length);
        if (written <= 0) _exit(90);
        text += written; length -= (size_t)written;
    }
}
HELPER void repeat(int fd, char byte, size_t count) {
    char block[4096];
    memset(block, byte, sizeof(block));
    while (count) {
        size_t length = count < sizeof(block) ? count : sizeof(block);
        ssize_t written = write(fd, block, length);
        if (written <= 0) _exit(90);
        count -= (size_t)written;
    }
}
/* Signal-safe: `gate` is prepared before any handler is installed. */
HELPER int connect_gate(void) {
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0 || connect(fd, (struct sockaddr *)&gate, sizeof(gate)) != 0) _exit(91);
    return fd;
}
HELPER void prepare(const char *argv0, const char *socket_name) {
    char path[1024];
    snprintf(path, sizeof(path), "%s", argv0);
    gate.sun_family = AF_UNIX;
    if (snprintf(gate.sun_path, sizeof(gate.sun_path), "%s/%s", dirname(path), socket_name) >=
        (int)sizeof(gate.sun_path)) _exit(92);
    char request[4096];
    while (read(0, request, sizeof(request)) > 0) {}
}
HELPER void on_term(void (*handler)(int)) {
    struct sigaction action = {0};
    action.sa_handler = handler;
    if (sigaction(SIGTERM, &action, NULL) != 0) _exit(93);
}

#if defined(INHERITED_STDERR)
/* A descendant keeps stderr open after the worker has exited. */
int main(int argc, char **argv) {
    (void)argc; prepare(argv[0], "holder.sock");
    pid_t child = fork();
    if (child < 0) return 94;
    if (child == 0) {
        int null = open("/dev/null", O_RDWR);
        if (null < 0 || dup2(null, 0) < 0 || dup2(null, 1) < 0) _exit(95);
        int holder = connect_gate();
        put(holder, "ready");
        char byte;
        if (read(holder, &byte, 1) == 1) put(holder, "done");
        _exit(0);
    }
    put(2, "live-prefix\n"); repeat(2, 'x', 12000); put(2, "\nfinal-worker-log\n");
    printf("{\"Ok\":{\"completed\":true,\"holderPid\":%d}}\n", (int)child);
    return fflush(stdout) == 0 ? 0 : 96;
}
#elif defined(CLEANUP_GATE)
/* SIGTERM starts cleanup that waits for one byte from the test's gate. */
static void cleanup(int signal_number) {
    (void)signal_number;
    int fd = connect_gate();
    put(2, "cleanup-started\n");
    char byte;
    if (read(fd, &byte, 1) != 1) _exit(97);
    put(2, "cleanup-completed\n");
    put(1, "{\"Ok\":{\"cleanup\":\"completed\"}}\n");
    _exit(0);
}
int main(int argc, char **argv) {
    (void)argc; prepare(argv[0], "cleanup.sock");
    on_term(cleanup);
    put(2, "worker-ready\n");
    for (;;) pause();
}
#elif defined(CANCEL_CLEANUP)
/* SIGTERM reports a completed cleanup at once. */
static void cleanup(int signal_number) {
    (void)signal_number;
    put(1, "{\"Ok\":{\"cleanup\":\"completed\"}}\n");
    _exit(0);
}
int main(int argc, char **argv) {
    (void)argc; prepare(argv[0], "unused.sock");
    on_term(cleanup);
    put(2, "ready\n");
    for (;;) pause();
}
#elif defined(FAILED_WORKER)
int main(int argc, char **argv) {
    (void)argc; prepare(argv[0], "unused.sock");
    repeat(2, 'x', 12000); put(2, "\nfailure-tail\n");
    return 7;
}
#elif defined(LARGE_LOG)
int main(int argc, char **argv) {
    (void)argc; prepare(argv[0], "unused.sock");
    repeat(2, 'x', 160000); put(2, "\n");
    put(1, "{\"Ok\":{\"completed\":true}}\n");
    return 0;
}
#else
#error "no fixture mode"
#endif
"#;

/// A compiled `mode` worker in a private directory, removed on drop.
pub(super) struct Fixture { program: crate::test_fixture::CFixture, pub(super) worker: std::path::PathBuf }
impl Fixture {
    pub(super) fn new(name: &str, mode: &str) -> Self {
        let program = crate::test_fixture::CFixture::compile(&format!("worker-{name}"), WORKER_SOURCE, &[mode]);
        Self { worker: program.program().to_owned(), program }
    }
    fn root(&self) -> &std::path::Path { self.program.program().parent().unwrap() }
}

#[tokio::test]
async fn exited_worker_finishes_with_complete_queued_logs_while_descendant_holds_stderr() {
    let fixture = Fixture::new("inherited-stderr", "INHERITED_STDERR");
    let listener = UnixListener::bind(fixture.root().join("holder.sock")).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let request = Request { timeout: 20, ..Default::default() };
    let call = call_executable(&request, fixture.worker.as_os_str(), None, Some(&sender));
    let holder = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut ready = [0; 5]; socket.read_exact(&mut ready).await.unwrap();
        assert_eq!(&ready, b"ready");
        socket
    };
    let pair = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(call, holder) }).await;
    // Dropping the gate socket on failure also releases its fixture-owned child.
    let (result, mut holder) = pair.unwrap();
    let result = result.unwrap();
    let pid = result["holderPid"].as_i64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "frontend must not kill a background owner");
    assert_eq!(result["completed"], true);
    let mut logs = String::new();
    while let Ok(event) = receiver.try_recv() { logs.push_str(event["text"].as_str().unwrap()); }
    assert_eq!(logs, format!("live-prefix\n{}\nfinal-worker-log\n", "x".repeat(12000)));
    holder.write_all(b"x").await.unwrap();
    let mut done = [0; 4]; holder.read_exact(&mut done).await.unwrap();
    assert_eq!(&done, b"done");
    assert_eq!(holder.read(&mut [0]).await.unwrap(), 0);
}

#[tokio::test]
async fn cancellation_keeps_streaming_and_waits_for_owned_worker_cleanup_gate() {
    let fixture = Fixture::new("cleanup-gate", "CLEANUP_GATE");
    let listener = UnixListener::bind(fixture.root().join("cleanup.sock")).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    let request = Request { timeout: 20, ..Default::default() };
    let completed = Arc::new(AtomicBool::new(false));
    let call = async {
        let result = call_executable(&request, fixture.worker.as_os_str(), Some(&cancel), Some(&sender)).await;
        completed.store(true, Ordering::SeqCst);
        result
    };
    let cleanup = async {
        assert_eq!(receiver.recv().await.unwrap()["text"], "worker-ready\n");
        cancel.cancel();
        let (mut gate, _) = listener.accept().await.unwrap();
        let started = receiver.recv().await.unwrap()["text"].as_str().unwrap().to_owned();
        if started != "cleanup-started\n" {
            // Report the fixture's whole failure, not only its first line.
            let mut failure = started;
            while let Ok(Some(event)) =
                tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv()).await
            {
                failure.push_str(event["text"].as_str().unwrap());
            }
            panic!("cleanup handler did not start cleanly:\n{failure}");
        }
        assert!(!completed.load(Ordering::SeqCst));
        gate.write_all(b"x").await.unwrap();
        assert_eq!(receiver.recv().await.unwrap()["text"], "cleanup-completed\n");
    };
    let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(call, cleanup) }).await.unwrap();
    assert_eq!(result.unwrap()["cleanup"], "completed");
}

#[tokio::test]
async fn failed_worker_preserves_diagnostic_output_and_unknown_outcome() {
    let fixture = Fixture::new("failed-worker", "FAILED_WORKER");
    let request = Request::default();
    let result = call_executable(&request, fixture.worker.as_os_str(), None, None).await.unwrap_err();
    assert_eq!(result.code, "outcomeUnknown");
    assert!(result.message.contains("failure-tail") && result.message.contains('7'));
}

#[tokio::test]
async fn saturated_log_channel_preserves_the_burst_and_final_diagnostic() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    // A paused renderer can leave every slot occupied before another stderr
    // burst arrives. Establish that boundary without relying on scheduling.
    for _ in 0..32 { sender.try_send(json!({"type":"log", "text":"queued\n"})).unwrap(); }
    let text = format!("{}\nfinal-cleanup-diagnostic\n", "x".repeat(160 * 1024));
    let done = tokio_util::sync::CancellationToken::new();
    let (reader, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let write = async { writer.write_all(text.as_bytes()).await.unwrap(); drop(writer); };
    let read = async {
        let result = live_error(reader, Some(&sender), false, &done).await;
        drop(sender);
        result.unwrap()
    };
    let consume = async {
        let mut logs = String::new();
        while let Some(event) = receiver.recv().await { logs.push_str(event["text"].as_str().unwrap()); }
        logs
    };
    let (saved, logs, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(read, consume, write)
    }).await.unwrap();
    assert!(logs == format!("{}{text}", "queued\n".repeat(32)), "stderr burst or final diagnostic was lost");
    assert!(saved.ends_with(b"final-cleanup-diagnostic\n"));
    assert!(saved.len() <= 8192);
}

#[tokio::test(start_paused = true)]
async fn worker_exit_during_renderer_backpressure_preserves_the_entire_queued_tail() {
    use std::{future::Future, task::Poll};
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    sender.try_send(json!({"text":"queued\n"})).unwrap();
    let text = format!("{}\nfinal-cleanup-diagnostic\n", "x".repeat(5000));
    let (reader, mut writer) = tokio::net::UnixStream::pair().unwrap();
    writer.write_all(text.as_bytes()).await.unwrap();
    reader.readable().await.unwrap();
    let done = tokio_util::sync::CancellationToken::new();
    done.cancel();
    let read = async {
        let result = live_error(reader, Some(&sender), false, &done).await.unwrap();
        drop(sender); result
    };
    tokio::pin!(read);
    std::future::poll_fn(|cx| {
        assert!(read.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
    // The old 250ms deadline expired while this renderer was blocked. Virtual
    // time establishes the condition without a scheduling-dependent sleep.
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    // Later descendant output is outside the completed worker's queue snapshot.
    writer.write_all(b"descendant-late-log\n").await.unwrap();
    let consume = async {
        let mut logs = String::new();
        while let Some(event) = receiver.recv().await { logs.push_str(event["text"].as_str().unwrap()); }
        logs
    };
    let (saved, logs) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(read, consume)
    }).await.unwrap();
    assert_eq!(logs, format!("queued\n{text}"));
    assert_eq!(saved, text.as_bytes());
    drop(writer); // Completion must not require the inherited descriptor's EOF.
}

#[tokio::test]
async fn disconnected_log_consumer_still_drains_and_reaps_the_worker() {
    let fixture = Fixture::new("closed-log-consumer", "LARGE_LOG");
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        call_executable(&Request::default(), fixture.worker.as_os_str(), None, Some(&sender))).await.unwrap().unwrap();
    assert_eq!(result["completed"], true);
}
