use crate::model::{Failure, Request, Result};
use serde_json::{Value, json};
use std::{
    ffi::{CStr, CString},
    io::{Read, Write},
    process::Stdio,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

unsafe extern "C" {
    fn proc_cancel_install() -> i32;
    fn hamn_control_query(profile: *const libc::c_char, result: *mut *mut libc::c_char) -> i32;
    fn hamn_control_free(result: *mut libc::c_char);
    fn hamn_control_start(profile: *const libc::c_char, cpu: u32, memory: u32, disk: u32) -> i32;
    fn hamn_control_configure(
        profile: *const libc::c_char,
        cpu: u32,
        memory: u32,
        disk: u32,
        create: i32,
    ) -> i32;
    fn hamn_control_stop(profile: *const libc::c_char) -> i32;
    fn hamn_control_delete(profile: *const libc::c_char) -> i32;
    fn hamn_control_migrate(profile: *const libc::c_char) -> i32;
    fn hamn_control_diagnostics(
        profile: *const libc::c_char,
        path: *const libc::c_char,
        result: *mut *mut libc::c_char,
    ) -> i32;
    fn hamn_control_update(manifest: *const libc::c_char) -> i32;
    fn hamn_control_uninstall(confirmed: i32) -> i32;
    fn log_last_error() -> *const libc::c_char;
    fn cli_set_invocation_path(path: *const libc::c_char);
}

fn query(profile: Option<&CString>) -> Result<Value> {
    let mut output = std::ptr::null_mut();
    let rc = unsafe {
        hamn_control_query(
            profile.map_or(std::ptr::null(), |p| p.as_ptr()),
            &mut output,
        )
    };
    if rc != 0 {
        return Err(Failure::new(
            "profileUnavailable",
            std::io::Error::last_os_error(),
        ));
    }
    if output.is_null() {
        return Err(Failure::new("coreProtocol", "missing C result"));
    }
    let value = serde_json::from_slice(unsafe { CStr::from_ptr(output) }.to_bytes());
    unsafe { hamn_control_free(output) };
    value.map_err(|e| Failure::new("coreProtocol", e))
}

fn execute(request: &Request) -> Result<Value> {
    request.validate()?;
    let profile = request
        .profile
        .as_ref()
        .map(|p| CString::new(p.as_str()))
        .transpose()
        .map_err(|e| Failure::new("invalidRequest", e))?;
    let pointer = profile.as_ref().map_or(std::ptr::null(), |p| p.as_ptr());
    let cpu = request.cpu.unwrap_or(0);
    let memory = request.memory.unwrap_or(0);
    let disk = request.disk.unwrap_or(0);
    let operation = request.operation();
    let path = request
        .path
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|e| Failure::new("invalidRequest", e))?;
    let manifest = request
        .manifest
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|e| Failure::new("invalidRequest", e))?;
    let mut output = std::ptr::null_mut();
    if operation == "vm list" {
        return query(None);
    }
    if operation == "vm status" {
        return query(profile.as_ref());
    }
    if operation == "vm env" {
        let status = query(profile.as_ref())?;
        return Ok(
            json!({"DOCKER_HOST":format!("unix://{}", status["dockerSocket"].as_str().unwrap_or_default())}),
        );
    }
    let rc = unsafe {
        match operation.as_str() {
            "vm migrate" => hamn_control_migrate(pointer),
            "vm diagnostics" => hamn_control_diagnostics(
                pointer,
                path.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                &mut output,
            ),
            "system update" => {
                hamn_control_update(manifest.as_ref().map_or(std::ptr::null(), |m| m.as_ptr()))
            }
            "system uninstall" => hamn_control_uninstall(i32::from(request.yes)),
            "vm start" => hamn_control_start(pointer, cpu, memory, disk),
            "vm create" | "vm configure" => hamn_control_configure(
                pointer,
                cpu,
                memory,
                disk,
                i32::from(operation == "vm create"),
            ),
            "vm stop" => hamn_control_stop(pointer),
            "vm delete" => hamn_control_delete(pointer),
            _ => return Err(Failure::new("unsupportedOperation", &operation)),
        }
    };
    if !output.is_null() {
        let value = serde_json::from_slice(unsafe { CStr::from_ptr(output) }.to_bytes());
        unsafe { hamn_control_free(output) };
        if rc == 0 {
            return value.map_err(|e| Failure::new("coreProtocol", e));
        }
    }
    if rc != 0 {
        let unknown = operation.starts_with("vm ") && query(profile.as_ref())
            .is_ok_and(|v| v["lastOperation"]["status"] == "outcomeUnknown");
        let message = unsafe { CStr::from_ptr(log_last_error()) }.to_string_lossy();
        return Err(Failure::new(
            if unknown {
                "outcomeUnknown"
            } else if rc == 130 {
                "cancelled"
            } else if rc == 3 {
                "restartRequired"
            } else if rc == 4 {
                "conflict"
            } else {
                "operationFailed"
            },
            if message.is_empty() {
                "C core operation failed"
            } else {
                &message
            },
        ));
    }
    if operation.starts_with("system ") {
        Ok(json!({"completed":true}))
    } else if operation == "vm delete" {
        Ok(json!({"deleted":true}))
    } else {
        query(profile.as_ref())
    }
}

fn protocol_descriptor(fd: i32) -> i32 {
    unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) }
}

pub fn worker() -> i32 {
    if unsafe { proc_cancel_install() } != 0 { return 1; }
    let mut input = Vec::new();
    let request = std::io::stdin()
        .take(65537)
        .read_to_end(&mut input)
        .map_err(|e| Failure::new("coreProtocol", e))
        .and_then(|_| {
            if input.len() > 65536 {
                return Err(Failure::new("coreProtocol", "request too large"));
            }
            serde_json::from_slice::<Request>(&input).map_err(|e| Failure::new("coreProtocol", e))
        });
    // Keep C printf/logging out of the machine protocol; never parse CLI text.
    // A VM supervisor outlives the worker. Its exec must close this protocol
    // descriptor, otherwise the frontend never observes EOF after completion.
    let saved = protocol_descriptor(libc::STDOUT_FILENO);
    if saved < 0 || unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        return 1;
    }
    if let Ok(path) = CString::new(
        std::env::args()
            .nth(2)
            .or_else(|| std::env::args().next())
            .unwrap_or_default(),
    ) {
        unsafe { cli_set_invocation_path(path.as_ptr()) };
    }
    let result = request.and_then(|r| execute(&r));
    unsafe {
        libc::fflush(std::ptr::null_mut());
        libc::dup2(saved, libc::STDOUT_FILENO);
        libc::close(saved);
    }
    let text = serde_json::to_vec(&result).expect("serializable worker result");
    if std::io::stdout().write_all(&text).is_err() {
        return 1;
    }
    0
}

async fn capture(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
    truncate: bool,
) -> std::io::Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(result);
        }
        let available = limit.saturating_sub(result.len());
        result.extend_from_slice(&buffer[..count.min(available)]);
        if count > available && !truncate {
            return Err(std::io::Error::other("worker response too large"));
        }
    }
}

pub async fn call(request: &Request) -> Result<Value> {
    let executable = std::env::current_exe().map_err(|e| Failure::new("coreUnavailable", e))?;
    let result = call_executable(request, executable.as_os_str(), None, None).await;
    if request.operation() == "vm start"
        && result.as_ref().is_err_and(|e| e.code == "restartRequired")
    {
        let invocation = std::env::args_os().next().unwrap_or_default();
        return call_executable(request, &invocation, None, None).await;
    }
    result
}

pub async fn call_control(
    request: &Request,
    cancel: &tokio_util::sync::CancellationToken,
    events: Option<&crate::stream::Events>,
) -> Result<Value> {
    if cancel.is_cancelled() { return Err(Failure::new("cancelled", "operation cancelled before dispatch")); }
    let executable = std::env::current_exe().map_err(|e| Failure::new("coreUnavailable", e))?;
    let result = call_executable(request, executable.as_os_str(), Some(cancel), events).await;
    if request.operation() == "vm start"
        && result.as_ref().is_err_and(|e| e.code == "restartRequired")
        && !cancel.is_cancelled()
    {
        let invocation = std::env::args_os().next().unwrap_or_default();
        return call_executable(request, &invocation, Some(cancel), events).await;
    }
    result
}

async fn live_error(
    mut reader: impl AsyncRead + std::os::fd::AsRawFd + Unpin,
    events: Option<&crate::stream::Events>,
    headless: bool,
    worker_done: &tokio_util::sync::CancellationToken,
) -> std::io::Result<Vec<u8>> {
    // Background SSH masters may inherit stderr. Only the exact worker owns
    // operation completion. After reaping, snapshot the queued byte count and
    // drain exactly that tail. Renderer backpressure cannot consume a deadline,
    // and a descendant cannot prolong completion by holding or writing stderr.
    let mut remaining = None;
    let mut saved = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        if remaining == Some(0) { return Ok(saved); }
        let capacity = remaining.unwrap_or(buffer.len()).min(buffer.len());
        let n = tokio::select! {
            biased;
            _ = worker_done.cancelled(), if remaining.is_none() => {
                let mut queued: libc::c_int = 0;
                if unsafe { libc::ioctl(reader.as_raw_fd(), libc::FIONREAD, &mut queued) } < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                remaining = Some(queued.max(0) as usize);
                continue;
            },
            n = reader.read(&mut buffer[..capacity]) => n?,
        };
        if n == 0 { return Ok(saved); }
        if let Some(remaining) = &mut remaining { *remaining -= n; }
        // Keep the final diagnostic if the worker exits without a response.
        // Logs already delivered to the UI are retained by its own bounded log.
        saved.extend_from_slice(&buffer[..n]);
        if saved.len() > 8192 { saved.drain(..saved.len() - 8192); }
        if headless { let _ = std::io::stderr().write_all(&buffer[..n]); }
        if let Some(events) = events.filter(|_| !headless) {
            // Backpressure preserves a burst when the renderer is temporarily
            // busy. A disconnected renderer must still allow worker reaping.
            let _ = events.send(json!({"type":"log", "text":String::from_utf8_lossy(&buffer[..n])})).await;
        }
    }
}

async fn call_executable(
    request: &Request, executable: &std::ffi::OsStr,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    events: Option<&crate::stream::Events>,
) -> Result<Value> {
    let mut child = tokio::process::Command::new(executable)
        .arg("__core-worker")
        .arg(std::env::args_os().next().unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(cancel.is_none())
        .spawn()
        .map_err(|e| Failure::new("coreUnavailable", e))?;
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&serde_json::to_vec(request).unwrap())
        .await
        .map_err(|e| Failure::new("coreProtocol", e))?;
    drop(input);
    let output = child.stdout.take().unwrap();
    let error = child.stderr.take().unwrap();
    let worker_done = tokio_util::sync::CancellationToken::new();
    let wait = async {
        let status = async {
            if let Some(cancel) = cancel {
                tokio::select! {
                    status = child.wait() => return status,
                    _ = cancel.cancelled() => {},
                    _ = tokio::time::sleep(std::time::Duration::from_secs(request.timeout)) => {},
                }
                // try_wait reaps an exited child. Otherwise its PID cannot be reused
                // before our wait, so SIGTERM targets this exact owned worker.
                if let Some(status) = child.try_wait()? { return Ok(status); }
                if let Some(pid) = child.id() { unsafe { libc::kill(pid as i32, libc::SIGTERM); } }
            }
            child.wait().await
        }.await;
        worker_done.cancel();
        status
    };
    let (status, output, error) = tokio::try_join!(
        wait,
        capture(output, 1024 * 1024, false),
        live_error(error, events, request.headless, &worker_done)
    )
    .map_err(|e| Failure::new("coreProtocol", e))?;
    if !status.success() {
        return Err(Failure::new(
            "outcomeUnknown",
            format!("worker {status}: {}", String::from_utf8_lossy(&error)),
        ));
    }
    serde_json::from_slice(&output).map_err(|e| Failure::new("coreProtocol", e))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;

    #[tokio::test]
    async fn cancellation_waits_for_the_owned_workers_cleanup_result() {
        use std::os::unix::fs::OpenOptionsExt;
        let path = std::env::temp_dir().join(format!("hamn-cancel-worker-{}.py", std::process::id()));
        let mut file = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o700).open(&path).unwrap();
        file.write_all(format!("#!{}\n", worker_lifetime_tests::python()).as_bytes()).unwrap();
        file.write_all(br#"import json, signal, sys
json.load(sys.stdin)
def cleanup(*_):
    print(json.dumps({'Ok': {'cleanup': 'completed'}}), flush=True)
    sys.exit(0)
signal.signal(signal.SIGTERM, cleanup)
print('ready', file=sys.stderr, flush=True)
signal.pause()
"#).unwrap();
        drop(file);
        let cancel = tokio_util::sync::CancellationToken::new();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        let request = Request { timeout: 20, ..Default::default() };
        let done = tokio_util::sync::CancellationToken::new();
        let call = async {
            let result = call_executable(&request, path.as_os_str(), Some(&cancel), Some(&sender)).await;
            done.cancel(); result
        };
        let trigger = async {
            tokio::select! {
                event = receiver.recv() => assert_eq!(event.unwrap()["text"], "ready\n"),
                _ = done.cancelled() => panic!("fixture worker exited before installing its cancellation handler"),
            }
            cancel.cancel();
        };
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(30), async { tokio::join!(call, trigger) }).await.unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(result.unwrap()["cleanup"], "completed");
    }

    #[test]
    fn background_exec_cannot_keep_worker_protocol_open() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let saved = protocol_descriptor(writer.as_raw_fd());
        assert!(saved >= 0);
        let saved = unsafe { OwnedFd::from_raw_fd(saved) };
        let mut child = std::process::Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(writer);
        drop(saved);
        // Parallel tests may briefly inherit this socket between fork and exec.
        // Await EOF while cat is still alive: a leaked descriptor still times out,
        // but an unrelated child completing exec must not make this check flaky.
        reader.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut fd = libc::pollfd { fd: reader.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let rc = unsafe { libc::poll(&mut fd, 1, remaining.as_millis() as i32) };
            if rc >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted || remaining.is_zero() { break; }
        }
        let result = std::io::Read::read(&mut reader, &mut [0]);
        let alive = child.try_wait().unwrap().is_none();
        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(alive);
        assert_eq!(result.unwrap(), 0, "EOF must not depend on daemon exit");
    }
}

#[cfg(test)]
#[path = "core_worker_tests.rs"]
mod worker_lifetime_tests;
