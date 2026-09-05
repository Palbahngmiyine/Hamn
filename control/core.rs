use crate::model::{Failure, Request, Result};
use serde_json::{Value, json};
use std::{
    ffi::{CStr, CString},
    io::{Read, Write},
    process::Stdio,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

unsafe extern "C" {
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
        let message = unsafe { CStr::from_ptr(log_last_error()) }.to_string_lossy();
        return Err(Failure::new(
            if rc == 3 {
                "restartRequired"
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

pub fn worker() -> i32 {
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
    let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
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
    let result = call_executable(request, executable.as_os_str()).await;
    if request.operation() == "vm start"
        && result.as_ref().is_err_and(|e| e.code == "restartRequired")
    {
        let invocation = std::env::args_os().next().unwrap_or_default();
        return call_executable(request, &invocation).await;
    }
    result
}

async fn call_executable(request: &Request, executable: &std::ffi::OsStr) -> Result<Value> {
    let mut child = tokio::process::Command::new(executable)
        .arg("__core-worker")
        .arg(std::env::args_os().next().unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
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
    let (status, output, error) = tokio::try_join!(
        child.wait(),
        capture(output, 1024 * 1024, false),
        capture(error, 8192, true)
    )
    .map_err(|e| Failure::new("coreProtocol", e))?;
    if !status.success() {
        return Err(Failure::new(
            "outcomeUnknown",
            String::from_utf8_lossy(&error),
        ));
    }
    serde_json::from_slice(&output).map_err(|e| Failure::new("coreProtocol", e))?
}
