//! Unlinked, read-only command input: no named body file survives cancellation.
use std::{fs::{File, OpenOptions}, io::{self, Seek, SeekFrom, Write}, os::fd::AsRawFd};
use std::os::unix::fs::OpenOptionsExt;

pub fn attach(command: &mut tokio::process::Command, body: &[u8]) -> io::Result<File> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(".hamn-input-{}-{sequence}", std::process::id()));
    let mut file = OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path)?;
    // Rust opens CLOEXEC; only this child's pre_exec will inherit the descriptor.
    std::fs::remove_file(path)?;
    file.write_all(body)?;
    file.seek(SeekFrom::Start(0))?;
    let fd = file.as_raw_fd();
    command.args(["--filename", &format!("/dev/fd/{fd}")]);
    unsafe { command.pre_exec(move || {
        if libc::fcntl(fd, libc::F_SETFD, 0) < 0 { return Err(io::Error::last_os_error()); }
        Ok(())
    }); }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn inherited_body_remains_readable_without_a_named_file() {
        let mut command = tokio::process::Command::new("/usr/bin/python3");
        command.args(["-c", "import os,sys; f=open(sys.argv[-1],'rb'); assert os.fstat(f.fileno()).st_nlink==0; sys.stdout.buffer.write(f.read())"]);
        let input = attach(&mut command, b"private delete preconditions").unwrap();
        let output = command.output().await.unwrap(); drop(input);
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(output.stdout, b"private delete preconditions");
    }
}
