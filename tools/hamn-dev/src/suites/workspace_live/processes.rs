//! Process observation without signalling, from libproc: births (PID and
//! start time), identities (with parent, executable UUID and path), child
//! lists and argument vectors. A process the test owns is recognized by its
//! identity, never by a PID alone, which the system may reuse.
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A process's PID and start time; no later process with that PID shares
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Birth {
    pub pid: i32,
    pub start_sec: u64,
    pub start_usec: u64,
}

/// A process as the transport checks identify it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub pid: i32,
    pub ppid: i32,
    pub start_sec: u64,
    pub start_usec: u64,
    /// The Mach-O UUID of the running executable, in hex.
    pub executable_uuid: String,
    pub path: String,
}

/// The process table, as the ownership guards read it; a fake stands in
/// for [`System`] in the guards' own checks.
pub trait Table {
    fn identity(&self, pid: i32) -> Result<Identity, String>;
    fn children(&self, pid: i32) -> Result<Vec<i32>, String>;
    fn argv(&self, pid: i32) -> Result<Vec<String>, String>;
}

/// The running system's process table.
pub struct System;

fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_bsdinfo is plain data, fully written on success.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is writable for `size` bytes.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (written == size).then_some(info)
}

fn own_process(info: &libc::proc_bsdinfo, pid: i32) -> bool {
    // SAFETY: getuid has no preconditions.
    info.pbi_pid as i32 == pid && info.pbi_uid == unsafe { libc::getuid() }
}

/// The birth of `pid`, or `None` when no such process exists. A process of
/// another user is never one the test started, so it fails loudly.
pub fn birth(pid: i32) -> Option<Birth> {
    let info = bsd_info(pid)?;
    assert!(own_process(&info, pid), "process {pid} belongs to another user");
    Some(Birth { pid, start_sec: info.pbi_start_tvsec, start_usec: info.pbi_start_tvusec })
}

/// Waits up to `timeout` until none of `births` names a live process. The
/// processes are not the test's children, so exits cannot be waited for;
/// their births are polled every 20 ms until the deadline.
pub fn gone(births: &[Birth], timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining: Vec<&Birth> =
            births.iter().filter(|expected| birth(expected.pid).as_ref() == Some(expected)).collect();
        if remaining.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("owned transport processes survived: {remaining:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Table for System {
    fn identity(&self, pid: i32) -> Result<Identity, String> {
        let info = bsd_info(pid).ok_or_else(|| format!("process {pid} is gone"))?;
        if !own_process(&info, pid) {
            return Err(format!("process {pid} belongs to another user"));
        }
        // SAFETY: rusage_info_v0 is plain data, fully written on success.
        let mut usage: libc::rusage_info_v0 = unsafe { std::mem::zeroed() };
        // SAFETY: flavor V0 writes one rusage_info_v0.
        if unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V0, (&raw mut usage).cast()) } != 0 {
            return Err(format!("proc_pid_rusage {pid}: {}", std::io::Error::last_os_error()));
        }
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is writable for its length.
        let length = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        if length <= 0 {
            return Err(format!("proc_pidpath {pid}: {}", std::io::Error::last_os_error()));
        }
        path.truncate(length as usize);
        Ok(Identity {
            pid,
            ppid: info.pbi_ppid as i32,
            start_sec: info.pbi_start_tvsec,
            start_usec: info.pbi_start_tvusec,
            executable_uuid: usage.ri_uuid.iter().map(|byte| format!("{byte:02x}")).collect(),
            path: String::from_utf8_lossy(&path).into_owned(),
        })
    }

    fn children(&self, pid: i32) -> Result<Vec<i32>, String> {
        let mut buffer = vec![0 as libc::pid_t; 256];
        let bytes = (buffer.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        // SAFETY: the buffer is writable for `bytes` bytes.
        let count = unsafe { libc::proc_listchildpids(pid, buffer.as_mut_ptr().cast(), bytes) };
        // A full buffer may have dropped children.
        if count < 0 || count as usize >= buffer.len() {
            return Err(format!("child inventory of {pid} failed or truncated"));
        }
        Ok(buffer.into_iter().filter(|&child| child > 0).collect())
    }

    fn argv(&self, pid: i32) -> Result<Vec<String>, String> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut buffer = vec![0u8; 1 << 20];
        let mut size = buffer.len();
        // SAFETY: mib holds three names; the buffer is writable for `size`.
        let result = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, buffer.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0)
        };
        if result != 0 {
            return Err(format!("KERN_PROCARGS2 {pid}: {}", std::io::Error::last_os_error()));
        }
        parse_procargs(&buffer[..size])
    }
}

/// The argument vector in a `KERN_PROCARGS2` buffer: a little-endian argc,
/// the executable path, NUL padding, then argc NUL-terminated arguments.
pub fn parse_procargs(raw: &[u8]) -> Result<Vec<String>, String> {
    let invalid = || "invalid process argument buffer".to_owned();
    let argc = u32::from_le_bytes(raw.get(..4).ok_or_else(invalid)?.try_into().expect("four bytes")) as usize;
    if argc == 0 || argc >= 4096 {
        return Err(invalid());
    }
    let mut offset = 4 + raw[4..].iter().position(|&byte| byte == 0).ok_or_else(invalid)? + 1;
    while *raw.get(offset).ok_or_else(invalid)? == 0 {
        offset += 1;
    }
    let args: Vec<String> = raw[offset..]
        .split(|&byte| byte == 0)
        .take(argc)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    if args.len() != argc {
        return Err(invalid());
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn a_running_child_is_identified_listed_and_its_arguments_read() {
        let mut child = Command::new("/bin/sleep").arg("30").stdin(Stdio::null()).spawn().unwrap();
        let pid = child.id() as i32;
        let identity = System.identity(pid).unwrap();
        assert_eq!(
            (identity.pid, identity.ppid, identity.path.as_str()),
            (pid, std::process::id() as i32, "/bin/sleep")
        );
        assert_eq!(identity.executable_uuid.len(), 32);
        let born = birth(pid).unwrap();
        assert_eq!((born.start_sec, born.start_usec), (identity.start_sec, identity.start_usec));
        assert!(System.children(std::process::id() as i32).unwrap().contains(&pid));
        assert_eq!(System.argv(pid).unwrap(), ["/bin/sleep", "30"]);
        assert!(gone(&[born], Duration::ZERO).unwrap_err().contains("survived"));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(birth(pid), None);
        gone(&[born], Duration::ZERO).unwrap();
        assert!(System.identity(pid).is_err());
    }

    #[test]
    fn procargs_parsing_requires_the_declared_arguments() {
        let mut raw = 2u32.to_le_bytes().to_vec();
        raw.extend_from_slice(b"/usr/bin/ssh\0\0\0\0ssh\0-F\0HOME=/x\0");
        assert_eq!(parse_procargs(&raw).unwrap(), ["ssh", "-F"]);
        let mut short = 3u32.to_le_bytes().to_vec();
        short.extend_from_slice(b"/bin/x\0\0a\0b");
        assert!(parse_procargs(&short).is_err());
        assert!(parse_procargs(&0u32.to_le_bytes()).is_err());
        assert!(parse_procargs(b"\x01\0").is_err());
        let mut unterminated = 1u32.to_le_bytes().to_vec();
        unterminated.extend_from_slice(b"/bin/x\0\0\0");
        assert!(parse_procargs(&unterminated).is_err());
    }
}
