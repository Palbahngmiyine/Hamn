//! Private installer operations embedded in the single shipped executable.
//! Runs before async/TUI initialization. Arguments are UTF-8 paths/identities;
//! stdout contains only requested machine-readable results, errors use stderr.
//! No VM/profile operations or external language runtimes. The upgrade subtree
//! owns bounded release HTTPS acquisition through the macOS system curl.
mod archive;
mod download;
mod files;
mod manifest;
mod progress;
mod receipt;
mod retention;
mod upgrade;

use std::path::Path;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// An error whose reason was already handed to the caller (for example the
/// updater's `<name>.reason` file); `run` exits non-zero without printing it.
#[derive(Debug)]
struct ReportedError(String);
impl std::fmt::Display for ReportedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ReportedError {}

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

pub fn run() -> i32 {
    let args = std::env::args_os()
        .skip(2)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "installer argument is not UTF-8".into())
        })
        .collect::<Result<Vec<_>>>();
    let quiet = args.as_ref().is_ok_and(|a| {
        a.first().map(String::as_str) == Some("receipt")
            && a.get(1).map(String::as_str) == Some("check")
    });
    let result = args.and_then(|args| dispatch(&args));
    match result {
        Ok(()) => 0,
        Err(error) => {
            if !quiet && !error.is::<ReportedError>() {
                eprintln!("hamn: {error}");
            }
            1
        }
    }
}

fn dispatch(args: &[String]) -> Result<()> {
    let values: Vec<&str> = args.iter().map(String::as_str).collect();
    match values.as_slice() {
        ["upgrade", rest @ ..] => upgrade::run(
            &rest
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )?,
        ["extract", source, destination] => println!(
            "{}",
            archive::extract(Path::new(source), Path::new(destination))?
        ),
        ["manifest", file, os, architecture] => manifest::fields(file, os, architecture)?,
        [
            "bootstrap-manifest",
            path,
            version,
            commit,
            host,
            host_hash,
            guest,
            guest_hash,
        ] => {
            manifest::bootstrap(path, version, commit, host, host_hash, guest, guest_hash)?;
        }
        [
            "bootstrap-manifest",
            path,
            version,
            commit,
            host,
            host_hash,
            guest,
            guest_hash,
            host_size,
            guest_size,
        ] => {
            manifest::bootstrap_v3(
                path,
                version,
                commit,
                host,
                host_hash,
                guest,
                guest_hash,
                host_size.parse()?,
                guest_size.parse()?,
            )?;
        }
        ["same-file", a, b] => require(
            files::same_file(Path::new(a), Path::new(b))?,
            "different files",
        )?,
        ["hash", path] => println!("{}", files::digest(Path::new(path))?),
        ["path-hash", path] => println!("{}", files::path_hash(path)),
        ["lock-prepare", path] => files::lock_prepare(Path::new(path))?,
        ["lock-same", path, fd] => files::lock_same(Path::new(path), fd.parse()?)?,
        ["lock-acquire", path, fd] => files::lock_acquire(Path::new(path), fd.parse()?)?,
        ["fd-identity", fd] => println!("{}", files::fd_identity(fd.parse()?)?),
        ["fd-lock", fd] => files::flock(fd.parse()?, false)?,
        ["recovery-root", target, cache] => {
            files::recovery_root(Path::new(target), Path::new(cache))?
        }
        [
            "receipt",
            mode,
            target,
            version,
            host_hash,
            guest_hash,
            cache,
        ] => {
            receipt::run(mode, target, version, host_hash, guest_hash, cache)?;
        }
        ["prune", bin, data, previous, source] => {
            retention::collect(Path::new(bin), Path::new(data), previous, source)?;
        }
        _ => return Err("invalid private installer operation or argument count".into()),
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    pub(crate) struct Temp(pub PathBuf);
    impl Temp {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "hamn-install-native-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}
