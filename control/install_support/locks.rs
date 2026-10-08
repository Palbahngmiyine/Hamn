//! Owner-only advisory locks that serialize installation transactions.
//!
//! Every lock is a permanent, owner-only (0600, one link) regular file,
//! locked with `flock(LOCK_EX)` on an open file description that this
//! process opened. Dropping the guard unlocks that description and closes
//! the descriptor. The unlock is explicit because a child that the process
//! forks while the lock is held shares the description until it ends or
//! execs, and closing our descriptor alone would leave the lock to that
//! child. A process that ends without dropping the guard, SIGKILL included,
//! releases the lock once no such child shares the description, so no
//! stale-lock protocol exists.
//! The path's identity (device, inode, owner, mode, link count) is compared
//! with the open descriptor before and after blocking, so a lock file
//! replaced meanwhile is refused rather than trusted.
//!
//! Order. A transaction takes, and holds until it ends:
//! 1. the transaction locks of both canonical roots,
//!    `BINDIR/.hamn-transaction.lock` and
//!    `DATAPARENT/.DATABASE.hamn-transaction.lock` (byte order);
//! 2. an update additionally takes `~/.hamn/cache/.hamn-upgrade.lock`;
//! 3. a generation install takes the install locks
//!    `BINDIR/.hamn-install.lock` and `DATAPARENT/.DATABASE.hamn-install.lock`
//!    (byte order).
//!
//! Recovery, publication and collection all run under (1), so an installer
//! waits for an updater's recovery and vice versa. The former shell
//! installer held the same files as descriptors 6/7 and 8/9.
use super::{Result, files, interrupt, require};
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
};

/// One held lock. The descriptor stays open (and the lock held) until drop.
pub(super) struct Held {
    file: File,
}

impl Drop for Held {
    fn drop(&mut self) {
        loop {
            // The descriptor is owned by this guard and remains open
            // throughout Drop. Unlock the shared description before File
            // closes our copy.
            if unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::Interrupted,
                "cannot unlock installation lock: {error}"
            );
        }
    }
}

/// How a lock file is validated and how failures are described.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Transaction and cache locks: the parent must be an owned 0700 or
    /// 0755 directory.
    Transaction,
    /// Install locks: the former installer's messages.
    Install,
}

type Identity = (u64, u64, u32, u32, u64);

fn identity(m: &fs::Metadata) -> Identity {
    (m.dev(), m.ino(), m.uid(), m.mode() & 0o7777, m.nlink())
}

/// The lock file as an owned 0600 single-link regular file.
fn valid(path: &Path) -> Result<Identity> {
    Ok(identity(&files::owned(path, false, Some(0o600))?))
}

fn acquire(path: &Path, kind: Kind) -> Result<Held> {
    let shown = path.display();
    if kind == Kind::Transaction {
        let parent = files::owned(files::parent(path)?, true, None)?;
        require(
            matches!(parent.mode() & 0o7777, 0o700 | 0o755),
            "unsafe transaction lock parent",
        )?;
    }
    if files::absent(path) {
        // Exclusive creation never follows or replaces an entry that
        // appeared meanwhile; validation below decides either way.
        let _ = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path);
    }
    let before = match (valid(path), kind) {
        (Ok(identity), _) => identity,
        (Err(error), Kind::Transaction) => return Err(error),
        (Err(_), Kind::Install) => {
            return Err(format!("refusing unsafe install lock path: {shown}").into());
        }
    };
    let changed = |stage: &str| -> Box<dyn std::error::Error> {
        match kind {
            Kind::Install => format!("install lock path changed while {stage}: {shown}").into(),
            Kind::Transaction => "transaction descriptor differs".into(),
        }
    };
    let file = OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let held = |file: &File| -> bool {
        valid(path).is_ok_and(|now| now == before)
            && file.metadata().is_ok_and(|open| identity(&open) == before)
    };
    if !held(&file) {
        return Err(changed("opening"));
    }
    loop {
        // flock locks the open file description, which a child forked from
        // now on shares; `Held` is what ends the lock for every holder.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(match kind {
                Kind::Install => format!("failed to acquire install lock: {shown}").into(),
                Kind::Transaction => error.into(),
            });
        }
        interrupt::check()?;
    }
    // Every return from here unlocks explicitly, the refusal below included.
    let lock = Held { file };
    if !held(&lock.file) {
        return Err(changed("locking"));
    }
    Ok(lock)
}

/// Two lock paths in byte order, which every acquirer uses (no deadlock).
fn ordered(one: PathBuf, two: PathBuf) -> [PathBuf; 2] {
    if two.as_os_str().as_bytes() < one.as_os_str().as_bytes() {
        [two, one]
    } else {
        [one, two]
    }
}

/// Canonical installation roots: `bindir` and `datadir`'s parent exist.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Roots {
    pub(super) bindir: PathBuf,
    pub(super) datadir: PathBuf,
    pub(super) data_parent: PathBuf,
    pub(super) data_base: String,
}

impl Roots {
    /// Refuses symbolic-link roots, creates `bindir` and `datadir`'s parent
    /// (0755 before umask) and resolves both to canonical paths. `datadir`
    /// itself may be absent. Its last component must be a real name.
    pub(super) fn prepare(bindir: &Path, datadir: &Path) -> Result<Self> {
        for root in [bindir, datadir] {
            if fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err("refusing symlinked transaction root".into());
            }
        }
        let text = datadir.as_os_str().as_bytes();
        let trimmed = &text[..text.iter().rposition(|c| *c != b'/').map_or(0, |i| i + 1)];
        let (parent, base) = match trimmed.iter().rposition(|c| *c == b'/') {
            Some(0) => (&b"/"[..], &trimmed[1..]),
            Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
            None => (&b"."[..], trimmed),
        };
        let base = std::str::from_utf8(base).map_err(|_| "data directory is not UTF-8")?;
        if matches!(base, "" | "." | "..") {
            return Err(format!(
                "refusing non-canonical data directory: {}",
                datadir.display()
            )
            .into());
        }
        let parent = Path::new(std::ffi::OsStr::from_bytes(parent));
        for directory in [bindir, parent] {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(directory)?;
        }
        let data_parent = fs::canonicalize(parent)?;
        files::utf8(&data_parent)?;
        let roots = Self {
            bindir: fs::canonicalize(bindir)?,
            datadir: data_parent.join(base),
            data_parent,
            data_base: base.to_owned(),
        };
        files::utf8(&roots.bindir)?;
        Ok(roots)
    }

    fn lock_pair(&self, suffix: &str) -> [PathBuf; 2] {
        ordered(
            self.bindir.join(format!(".hamn-{suffix}.lock")),
            self.data_parent
                .join(format!(".{}.hamn-{suffix}.lock", self.data_base)),
        )
    }
}

/// The transaction locks of both roots. Holding this value is the proof,
/// required by installation, recovery and collection, that no other
/// transaction can change these roots.
pub(super) struct Transaction {
    _locks: Vec<Held>,
    roots: Roots,
}

impl Transaction {
    pub(super) fn acquire(roots: &Roots) -> Result<Self> {
        let mut locks = Vec::new();
        for path in roots.lock_pair("transaction") {
            locks.push(
                acquire(&path, Kind::Transaction)
                    .map_err(|error| format!("{}: {error}", path.display()))?,
            );
        }
        Ok(Self {
            _locks: locks,
            roots: roots.clone(),
        })
    }

    pub(super) fn roots(&self) -> &Roots {
        &self.roots
    }
}

/// The install locks of both roots, held while one generation is published.
pub(super) struct Install {
    _locks: Vec<Held>,
}

impl Install {
    /// Requires `transaction` (lock order); refuses unsafe lock files.
    pub(super) fn acquire(transaction: &Transaction) -> Result<Self> {
        let mut locks = Vec::new();
        for path in transaction.roots.lock_pair("install") {
            locks.push(acquire(&path, Kind::Install)?);
        }
        Ok(Self { _locks: locks })
    }
}

/// The per-HOME updater lock (`~/.hamn/cache/.hamn-upgrade.lock`), taken
/// after the transaction locks. It serializes updaters of one HOME even
/// across different install roots, which share one journal path.
pub(super) fn cache(path: &Path, _transaction: &Transaction) -> Result<Held> {
    acquire(path, Kind::Transaction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_support::test_support::Temp;
    use std::os::unix::fs::PermissionsExt;

    fn roots(t: &Temp) -> Roots {
        Roots::prepare(&t.0.join("bin"), &t.0.join("share/hamn/src")).unwrap()
    }

    fn try_lock(path: &Path) -> bool {
        let file = OpenOptions::new().append(true).open(path).unwrap();
        // The probe owns its own description; LOCK_NB never blocks. It
        // unlocks before closing: a child that another test forks meanwhile
        // would otherwise keep the probe's lock on that description.
        let free = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 };
        assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) }, 0);
        free
    }

    /// A child of this process that does not exec: it shares every open file
    /// description that the process had when it forked, until it ends.
    struct ForkedChild {
        pid: libc::pid_t,
        release: libc::c_int,
    }

    impl ForkedChild {
        fn fork() -> Self {
            let mut release = [0; 2];
            let mut ready = [0; 2];
            assert_eq!(unsafe { libc::pipe(release.as_mut_ptr()) }, 0);
            assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
            for fd in release.into_iter().chain(ready) {
                assert_eq!(
                    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                    0
                );
            }
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0);
            if pid == 0 {
                // A multithreaded test runner may own Rust/allocator locks at
                // fork. The child uses only async-signal-safe libc and exits
                // without Drop.
                unsafe {
                    libc::close(release[1]);
                    libc::close(ready[0]);
                    let byte = 1_u8;
                    if libc::write(ready[1], (&byte as *const u8).cast(), 1) != 1 {
                        libc::_exit(2);
                    }
                    let mut event = libc::pollfd {
                        fd: release[0],
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    let released = libc::poll(&mut event, 1, 5000) > 0;
                    libc::_exit(if released { 0 } else { 3 });
                }
            }
            unsafe {
                libc::close(release[0]);
                libc::close(ready[1]);
            }
            let child = Self {
                pid,
                release: release[1],
            };
            let mut event = libc::pollfd {
                fd: ready[0],
                events: libc::POLLIN,
                revents: 0,
            };
            let mut byte = 0_u8;
            let started = unsafe { libc::poll(&mut event, 1, 5000) } > 0
                && unsafe { libc::read(ready[0], (&mut byte as *mut u8).cast(), 1) } == 1;
            unsafe { libc::close(ready[0]) };
            assert!(started, "the forked child did not start");
            child
        }

        /// Lets the child end and requires that it ended as asked. A byte,
        /// not end-of-file: other tests' forked children inherit the writer.
        fn end(self) {
            let byte = 1_u8;
            let asked = unsafe { libc::write(self.release, (&byte as *const u8).cast(), 1) };
            let mut status = 0;
            let reaped = loop {
                let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
                let interrupted =
                    std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
                if result >= 0 || !interrupted {
                    break result;
                }
            };
            unsafe { libc::close(self.release) };
            std::mem::forget(self);
            assert_eq!(asked, 1);
            assert!(
                reaped >= 0 && libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                "the forked child ended with status {status}"
            );
        }
    }

    impl Drop for ForkedChild {
        /// A failed test must not leave the child or its descriptor behind.
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
                libc::waitpid(self.pid, std::ptr::null_mut(), 0);
                libc::close(self.release);
            }
        }
    }

    #[test]
    fn forked_child_cannot_extend_released_transaction_and_install_locks() {
        let t = Temp::new();
        let r = roots(&t);
        let transaction = Transaction::acquire(&r).unwrap();
        let install = Install::acquire(&transaction).unwrap();
        let paths: Vec<PathBuf> = r
            .lock_pair("transaction")
            .into_iter()
            .chain(r.lock_pair("install"))
            .collect();
        let child = ForkedChild::fork();
        assert!(paths.iter().all(|path| !try_lock(path)));
        drop(install);
        drop(transaction);
        // The child still holds the inherited open file descriptions. The end
        // of a transaction must not depend on that child's lifetime.
        let free: Vec<bool> = paths.iter().map(|path| try_lock(path)).collect();
        child.end();
        assert_eq!(free, [true; 4], "{paths:?}");
        // Another transaction can start at once, as an installer waiting on
        // these locks would.
        drop(Install::acquire(&Transaction::acquire(&r).unwrap()).unwrap());
    }

    #[test]
    fn roots_are_canonical_and_reject_links_and_dot_names() {
        let t = Temp::new();
        let r = roots(&t);
        let base = fs::canonicalize(&t.0).unwrap();
        assert_eq!(r.bindir, base.join("bin"));
        assert_eq!(
            (r.data_parent.clone(), r.data_base.as_str()),
            (base.join("share/hamn"), "src")
        );
        assert!(
            !r.datadir.exists(),
            "prepare must not create the data directory itself"
        );
        for bad in ["share/..", "share/.", "share/./"] {
            assert!(
                Roots::prepare(&t.0.join("bin"), &t.0.join(bad)).is_err(),
                "{bad}"
            );
        }
        assert!(Roots::prepare(&t.0.join("bin"), Path::new("/")).is_err());
        std::os::unix::fs::symlink(t.0.join("bin"), t.0.join("alias")).unwrap();
        assert!(Roots::prepare(&t.0.join("alias"), &t.0.join("data")).is_err());
    }

    #[test]
    fn transaction_and_install_locks_are_held_until_drop_in_byte_order() {
        let t = Temp::new();
        let r = roots(&t);
        let transaction = Transaction::acquire(&r).unwrap();
        let [first, second] = r.lock_pair("transaction");
        assert!(first.as_os_str().as_bytes() < second.as_os_str().as_bytes());
        assert!(!try_lock(&first) && !try_lock(&second));
        let install = Install::acquire(&transaction).unwrap();
        for path in r.lock_pair("install") {
            assert!(files::safe_private_regular(&path) && !try_lock(&path));
        }
        drop(install);
        drop(transaction);
        assert!(try_lock(&first) && try_lock(&second));
    }

    #[test]
    fn unsafe_lock_files_are_refused_without_following_or_changing_them() {
        let t = Temp::new();
        let r = roots(&t);
        let target = t.0.join("target");
        fs::write(&target, "keep").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let [first, _] = r.lock_pair("install");
        std::os::unix::fs::symlink(&target, &first).unwrap();
        let transaction = Transaction::acquire(&r).unwrap();
        let error = Install::acquire(&transaction).err().unwrap().to_string();
        assert!(
            error.starts_with("refusing unsafe install lock path"),
            "{error}"
        );
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_file(&first).unwrap();
        // A second link to a lock file makes its identity untrustworthy.
        let [one, _] = r.lock_pair("install");
        fs::write(&one, "").unwrap();
        fs::set_permissions(&one, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&one, t.0.join("second-link")).unwrap();
        assert!(Install::acquire(&transaction).is_err());
        drop(transaction);
        let [transaction_lock, _] = r.lock_pair("transaction");
        fs::remove_file(&transaction_lock).unwrap();
        std::os::unix::fs::symlink(&target, &transaction_lock).unwrap();
        assert!(Transaction::acquire(&r).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
    }
}
