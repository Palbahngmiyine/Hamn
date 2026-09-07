//! Owned-worker completion must not depend on background stderr writers.
use super::*;
use std::{os::unix::fs::OpenOptionsExt, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use tokio::net::UnixListener;

pub(super) fn python() -> String {
    let output = std::process::Command::new("python3").args(["-c", "import sys; print(sys.executable)"]).output().unwrap();
    assert!(output.status.success(), "cannot resolve fixture interpreter");
    let path = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    assert!(!path.is_empty() && !path.contains('\n'));
    path
}

struct Fixture { root: std::path::PathBuf, worker: std::path::PathBuf }
impl Fixture {
    fn new(name: &str, script: &str) -> Self {
        let root = std::env::temp_dir().join(format!("hamn-worker-{name}-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let worker = root.join("worker.py");
        let mut file = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o700).open(&worker).unwrap();
        file.write_all(format!("#!{}\n", python()).as_bytes()).unwrap();
        file.write_all(b"import json, os, signal, socket, subprocess, sys\nfrom pathlib import Path\nroot = Path(__file__).parent\njson.load(sys.stdin)\n").unwrap();
        file.write_all(script.as_bytes()).unwrap();
        Self { root, worker }
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); } }

#[tokio::test]
async fn exited_worker_finishes_with_complete_queued_logs_while_descendant_holds_stderr() {
    let fixture = Fixture::new("inherited-stderr", r#"
code = 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall(b"ready"); data=s.recv(1); s.sendall(b"done") if data else None'
child = subprocess.Popen([sys.executable, '-c', code, str(root / 'holder.sock')],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
print('live-prefix\n' + 'x' * 12000 + '\nfinal-worker-log', file=sys.stderr, flush=True)
print(json.dumps({'Ok': {'completed': True, 'holderPid': child.pid}}), flush=True)
"#);
    let listener = UnixListener::bind(fixture.root.join("holder.sock")).unwrap();
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
    let fixture = Fixture::new("cleanup-gate", r#"
def cleanup(*_):
    gate = socket.socket(socket.AF_UNIX)
    gate.connect(str(root / 'cleanup.sock'))
    print('cleanup-started', file=sys.stderr, flush=True)
    gate.recv(1)
    print('cleanup-completed', file=sys.stderr, flush=True)
    print(json.dumps({'Ok': {'cleanup': 'completed'}}), flush=True)
    sys.exit(0)
signal.signal(signal.SIGTERM, cleanup)
print('worker-ready', file=sys.stderr, flush=True)
signal.pause()
"#);
    let listener = UnixListener::bind(fixture.root.join("cleanup.sock")).unwrap();
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
        assert_eq!(receiver.recv().await.unwrap()["text"], "cleanup-started\n");
        assert!(!completed.load(Ordering::SeqCst));
        gate.write_all(b"x").await.unwrap();
        assert_eq!(receiver.recv().await.unwrap()["text"], "cleanup-completed\n");
    };
    let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(call, cleanup) }).await.unwrap();
    assert_eq!(result.unwrap()["cleanup"], "completed");
}

#[tokio::test]
async fn failed_worker_preserves_diagnostic_output_and_unknown_outcome() {
    let fixture = Fixture::new("failed-worker", "print('x' * 12000 + '\\nfailure-tail', file=sys.stderr, flush=True)\nsys.exit(7)\n");
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
    let read = async {
        let result = live_error(text.as_bytes(), Some(&sender), false, &done).await;
        drop(sender);
        result.unwrap()
    };
    let consume = async {
        let mut logs = String::new();
        while let Some(event) = receiver.recv().await { logs.push_str(event["text"].as_str().unwrap()); }
        logs
    };
    let (saved, logs) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(read, consume)
    }).await.unwrap();
    assert!(logs == format!("{}{text}", "queued\n".repeat(32)), "stderr burst or final diagnostic was lost");
    assert!(saved.ends_with(b"final-cleanup-diagnostic\n"));
    assert!(saved.len() <= 8192);
}

#[tokio::test]
async fn disconnected_log_consumer_still_drains_and_reaps_the_worker() {
    let fixture = Fixture::new("closed-log-consumer", "print('x' * 160000, file=sys.stderr, flush=True)\nprint(json.dumps({'Ok': {'completed': True}}), flush=True)\n");
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        call_executable(&Request::default(), fixture.worker.as_os_str(), None, Some(&sender))).await.unwrap().unwrap();
    assert_eq!(result["completed"], true);
}
