//! Collection requires inherited locks for both canonical install roots. Keep
//! active, predecessor, caller, open-file and recovery references. Unknown
//! ownership/legacy generations remain; marker-last retirement is retryable.
use super::{Result, files, manifest::hexadecimal, require};
use std::{
    fs,
    io::{ErrorKind, Read},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn children(path: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(path)?
        .map(|p| p.map(|p| p.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}
fn tree_owned(path: &Path) -> Result<bool> {
    let m = fs::symlink_metadata(path)?;
    if files::owned(path, m.is_dir(), None).is_err() || m.mode() & 0o022 != 0 {
        return Ok(false);
    }
    if m.is_dir() {
        for child in children(path)? {
            if !tree_owned(&child)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
fn pending(cache: &Path) -> Result<bool> {
    match fs::symlink_metadata(cache) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
        Ok(_) => (),
    }
    let m = match files::owned(cache, true, None) {
        Ok(m) => m,
        Err(_) => return Ok(true),
    };
    if m.mode() & 0o022 != 0 {
        return Ok(true);
    }
    Ok(children(cache)?.iter().any(|p| {
        p.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".hamn-update-")
    }))
}
fn recovery_pending(path: &Path) -> Result<bool> {
    let value = files::text(path)?;
    if !value.starts_with('/')
        || value.contains('\n')
        || path.file_name().unwrap()
            != format!(".hamn-recovery-root-{}", files::hash(value.as_bytes())).as_str()
    {
        return Ok(true);
    }
    pending(Path::new(&value))
}
fn generation_name(name: &str) -> bool {
    name.is_ascii()
        && name.len() == 71
        && hexadecimal(&name[..64], 64)
        && name.as_bytes()[64] == b'-'
        && name.as_bytes()[65..].iter().all(u8::is_ascii_alphanumeric)
}
fn generation_target(path: &Path, root: &Path) -> bool {
    path.file_name().is_some_and(|p| p == "hamn")
        && path.parent().is_some_and(|p| {
            p.file_name().is_some_and(|p| p == "bin")
                && p.parent().is_some_and(|g| {
                    g.parent() == Some(root)
                        && g.file_name()
                            .and_then(|p| p.to_str())
                            .is_some_and(generation_name)
                })
        })
}
fn erase(path: &Path) -> Result<()> {
    for p in children(path)? {
        if p.file_name().unwrap() == ".hamn-generation" {
            continue;
        }
        if fs::symlink_metadata(&p)?.is_dir() {
            erase(&p)?;
        } else {
            fs::remove_file(p)?;
        }
    }
    match fs::remove_file(path.join(".hamn-generation")) {
        Ok(()) => (),
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    fs::remove_dir(path)?;
    Ok(())
}

/// Drain both streams concurrently; timeout kills and reaps the scanner process
/// group, rather than leaving a child alive behind an outer async deadline.
fn scan(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let readers: Vec<_> = [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr)]
        .into_iter()
        .enumerate()
        .map(|(index, mut pipe)| {
            let send = send.clone();
            thread::spawn(move || {
                let mut data = Vec::new();
                let result = pipe.read_to_end(&mut data).map(|_| data);
                let _ = send.send((index, result));
            })
        })
        .collect();
    drop(send);
    let deadline = Instant::now() + timeout;
    let mut output = [Vec::new(), Vec::new()];
    let mut complete = true;
    for _ in 0..2 {
        match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((index, Ok(data))) => output[index] = data,
            _ => {
                complete = false;
                break;
            }
        }
    }
    if !complete {
        // Do not reap the leader until pipe completion/deadline: its PID still
        // identifies this exclusively owned process group, including descendants.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    // Pipes can close before a child finishes. Retain a bounded wait in that case.
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            complete = false;
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    for reader in readers {
        reader.join().map_err(|_| "scanner reader failed")?;
    }
    require(
        complete && status.success() && output[1].is_empty(),
        "open-file scan unavailable or incomplete",
    )?;
    Ok(std::mem::take(&mut output[0]))
}

fn open_files(timeout: Duration) -> Result<Vec<String>> {
    let stdout = scan(
        Command::new("/usr/sbin/lsof").args(["-nP", "-F", "n"]),
        timeout,
    )?;
    let mut files = Vec::new();
    let mut process_files = Vec::new();
    let text = String::from_utf8(stdout)?;
    for line in text.lines().chain(std::iter::once("p")) {
        if line.starts_with('p') {
            require(
                !process_files
                    .iter()
                    .any(|p: &String| p.ends_with("/scripts/update-host.sh"))
                    || process_files
                        .iter()
                        .any(|p| p.ends_with(".hamn-transaction.lock")),
                "legacy updater is still running",
            )?;
            process_files.clear();
        } else if let Some(path) = line.strip_prefix('n').filter(|p| p.starts_with('/')) {
            files.push(path.to_owned());
            process_files.push(path.to_owned());
        }
    }
    Ok(files)
}

pub(super) fn collect(bin: &Path, data: &Path, previous: &str, source: &str) -> Result<()> {
    let mut locks = [
        bin.join(".hamn-transaction.lock"),
        files::parent(data)?.join(format!(
            ".{}.hamn-transaction.lock",
            data.file_name()
                .and_then(|n| n.to_str())
                .ok_or("invalid data path")?
        )),
    ];
    locks.sort();
    for (fd, path) in (6..=7).zip(locks) {
        files::lock_same(&path, fd)?;
        files::flock(fd, true)?;
    }
    let root = data.join(".hamn-generations");
    files::owned(data, true, Some(0o755))?;
    files::owned(&root, true, Some(0o755))?;
    let root_text = root.to_str().ok_or("invalid generation path")?;
    require(
        root_text
            .bytes()
            .all(|c| (32..=126).contains(&c) && c != b'\\'),
        "generation path cannot be matched safely in process output",
    )?;
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    if pending(&PathBuf::from(home).join(".hamn/cache"))? {
        eprintln!("hamn: generation cleanup deferred while recovery metadata exists");
        return Ok(());
    }
    let active = fs::read_link(bin.join("hamn"))?;
    require(
        generation_target(&active, &root),
        "active link is outside managed generation root",
    )?;
    let mut keep = vec![
        active.clone(),
        PathBuf::from(previous),
        PathBuf::from(source),
    ];
    let predecessor = files::parent(files::parent(&active)?)?.join(".hamn-previous-target");
    match fs::symlink_metadata(&predecessor) {
        Ok(_) => {
            files::owned(&predecessor, false, Some(0o600))?;
            let value = files::text(&predecessor)?;
            let target = PathBuf::from(value.trim_end_matches('\n'));
            require(
                value.ends_with('\n')
                    && value.bytes().filter(|c| *c == b'\n').count() == 1
                    && generation_target(&target, &root),
                "invalid predecessor reference",
            )?;
            keep.push(target);
        }
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let opened = open_files(Duration::from_secs(30))?;
    let bin_id = files::path_hash(bin.to_str().ok_or("invalid bin path")?);
    let data_id = files::path_hash(data.to_str().ok_or("invalid data path")?);
    for mut path in children(&root)? {
        let leaf = path
            .file_name()
            .and_then(|p| p.to_str())
            .unwrap_or_default()
            .to_owned();
        let retired = leaf.starts_with(".retired-");
        let name = leaf.strip_prefix(".retired-").unwrap_or(&leaf);
        if !generation_name(name) {
            continue;
        }
        let result = (|| -> Result<()> {
            if !tree_owned(&path)?
                || keep.iter().any(|p| p.starts_with(&path))
                || opened.iter().any(|p| Path::new(p).starts_with(&path))
            {
                return Ok(());
            }
            if retired && children(&path)?.is_empty() {
                fs::remove_dir(&path)?;
                return Ok(());
            }
            let marker = path.join(".hamn-generation");
            if files::owned(&marker, false, Some(0o600)).is_err() {
                return Ok(());
            }
            let expected = format!(
                "version=1\nbinary_sha256={}\nbindir_id={bin_id}\ndatadir_id={data_id}\n",
                &name[..64]
            );
            if files::text(&marker)? != expected {
                return Ok(());
            }
            for p in children(&path)? {
                if p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".hamn-recovery-root-")
                    && recovery_pending(&p)?
                {
                    return Ok(());
                }
            }
            if !retired {
                let retention = path.join(".hamn-retention");
                if files::owned(&retention, false, Some(0o600)).is_err()
                    || files::text(&retention)? != "version=1\n"
                {
                    return Ok(());
                }
                let binary = path.join("bin/hamn");
                files::owned(&binary, false, Some(0o755))?;
                if files::digest(&binary)? != name[..64] {
                    return Ok(());
                }
                let destination = root.join(format!(".retired-{name}"));
                match fs::symlink_metadata(&destination) {
                    Err(e) if e.kind() == ErrorKind::NotFound => (),
                    _ => return Ok(()),
                }
                fs::rename(&path, &destination)?;
                path = destination;
            }
            erase(&path)?;
            println!("hamn: removed obsolete generation {name}");
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("hamn: generation cleanup deferred: {error}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scanner_deadline_covers_exited_leader_and_inherited_pipes() {
        let start = Instant::now();
        let result = scan(
            Command::new("/bin/sh").args(["-c", "sleep 30 & exit 0"]),
            Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn scanner_failure_and_closed_pipes_are_bounded() {
        for script in ["echo error >&2", "exit 1", "exec 1>&- 2>&-; sleep 30"] {
            assert!(
                scan(
                    Command::new("/bin/sh").args(["-c", script]),
                    Duration::from_millis(100)
                )
                .is_err()
            );
        }
        assert_eq!(
            scan(
                Command::new("/bin/echo").arg("result"),
                Duration::from_secs(2)
            )
            .unwrap(),
            b"result\n"
        );
    }
    #[test]
    fn unicode_generation_names_do_not_panic() {
        assert!(!generation_name(&format!("{}é-12345", "a".repeat(63))));
    }
}
