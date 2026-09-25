//! Private installer operations embedded in the single shipped executable
//! (`hamn __install-support ...`). Runs before async/TUI initialization.
//! Arguments are UTF-8 paths/identities; stdout contains only requested
//! machine-readable results or the installer's summary lines, errors use
//! stderr. No VM/profile operations and no external language runtimes: the
//! only programs run are the macOS system `curl`, `sw_vers` and `lsof`, and
//! Hamn executables' `--version`.
//!
//! Operations:
//! - `install HAMN_BINARY BINDIR DATADIR`: publish a source build as a
//!   managed generation (`make install`); see `generation`.
//! - `update ...`: one verified release transaction (`hamn upgrade`, the
//!   release installer); see `update`.
//! - `prune BINDIR DATADIR [KEEP_TARGET...]`: collect obsolete generations
//!   under the transaction locks; see `retention`.
//! - `extract`, `bootstrap-manifest`, `same-file`: the release installer's
//!   steps before the updater runs.
//! - `upgrade ...`: manifest, check, acquisition and automatic-check
//!   operations; see `upgrade`.
mod archive;
mod download;
mod files;
mod generation;
mod interrupt;
mod journal;
mod locks;
mod manifest;
mod progress;
mod receipt;
mod retention;
mod update;
mod upgrade;

use std::{io::Write, path::Path};
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

/// One diagnostic line on stderr; a closed stream is not an error.
fn diagnostic(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
}

pub fn run() -> i32 {
    let args = std::env::args_os()
        .skip(2)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "installer argument is not UTF-8".into())
        })
        .collect::<Result<Vec<_>>>();
    let args = match args {
        Ok(args) => args,
        Err(error) => {
            diagnostic(&format!("hamn: {error}"));
            return 1;
        }
    };
    match args.split_first() {
        Some((operation, rest)) if operation == "update" => return update::run(rest),
        Some((operation, rest)) if operation == "install" => return install(rest),
        _ => {}
    }
    match dispatch(&args) {
        Ok(()) => 0,
        Err(error) => {
            if !error.is::<ReportedError>() {
                diagnostic(&format!("hamn: {error}"));
            }
            1
        }
    }
}

/// Collects after a committed installation and reports what happened;
/// failure only defers cleanup.
fn collect_reporting(transaction: &locks::Transaction, keep: &[&str]) -> Result<()> {
    let collection = retention::collect(transaction, keep)?;
    for name in collection.removed {
        println!("hamn: removed obsolete generation {name}");
    }
    for deferred in collection.deferred {
        diagnostic(&format!("hamn: {deferred}"));
    }
    Ok(())
}

/// `install HAMN_BINARY BINDIR DATADIR` (`make install`).
fn install(args: &[String]) -> i32 {
    let [source, bindir, datadir] = args else {
        diagnostic("usage: hamn __install-support install HAMN_BINARY BINDIR DATADIR");
        return 2;
    };
    let installed = (|| -> Result<generation::Installed> {
        let roots = locks::Roots::prepare(Path::new(bindir), Path::new(datadir))?;
        generation::refuse_overlap(&roots)?;
        let transaction = locks::Transaction::acquire(&roots)?;
        let payload = generation::Payload {
            binary: source.into(),
            pointer: None,
        };
        let installed = generation::install(&payload, &transaction, None)?;
        // Collection is best effort after the durable public commit. Never
        // roll back a successful installation because an obsolete generation
        // could not be removed.
        if let Err(error) = collect_reporting(&transaction, &[]) {
            diagnostic(&format!("hamn: {error}"));
            diagnostic("hamn: obsolete generation cleanup deferred");
        }
        Ok(installed)
    })();
    match installed {
        Ok(installed) => {
            println!("installed: {} -> {}", installed.link.display(), installed.target);
            println!("verify: hamn --headless vm start --profile default --yes");
            println!("Docker connection: hamn --headless vm env --profile default");
            0
        }
        Err(error) => {
            diagnostic(&format!("hamn: {error}"));
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
        ["prune", bindir, datadir, keep @ ..] => {
            let roots = locks::Roots::prepare(Path::new(bindir), Path::new(datadir))?;
            let transaction = locks::Transaction::acquire(&roots)?;
            collect_reporting(&transaction, keep)?;
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
