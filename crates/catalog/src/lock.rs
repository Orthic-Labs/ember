//! One process per library: an exclusive OS lock on `<library>/catalog.lock`, held for as long as
//! the library is open (issue #99).
//!
//! Two processes on the same library (the app and `lightcraft-cli --library`, two app instances,
//! two computers on a shared folder) would each keep their own catalog and `seq` counter, and
//! whichever wrote the last snapshot would silently drop the other's edits. So opening a library
//! takes the lock first, and a second opener gets [`LockError::InUse`] instead.
//!
//! - The lock and the owner note are `rightkit-fs`'s ([`rightkit_fs::FileLock`], `flock` on Unix,
//!   `LockFileEx` on Windows). It is `LockKind::Flock` because every shipped Ember takes `flock`,
//!   and the default `fcntl` record lock does not exclude a `flock` holder. The lock is released
//!   when the [`LibraryLock`] is dropped (explicitly, so a process another thread is spawning
//!   cannot keep it busy, issue #680) and, after a crash or a kill, by the OS. A `catalog.lock`
//!   file left behind is never "stale": only a lock that's actually held refuses the open.
//! - Who holds it (process id, computer, program) is in `catalog.lock.owner`, for the error
//!   message. It's only a hint: it may be missing, left over from a crash, or written by an older
//!   Ember in another shape (then the message says "another Ember program").
//! - File systems without lock support (some network shares), or a lock file that can't be opened:
//!   the library opens unlocked, with a warning (Ember's policy, not `rightkit-fs`'s: refusing it
//!   would lock the user out of their own library). Locks over SMB/NFS are advisory and depend on
//!   the server; the owner file is then the only hint.
//! - Browser storage (OPFS) has no file locks: see the web host for its own guard.

use std::path::{Path, PathBuf};

/// The lock file in the library directory.
pub const LOCK: &str = "catalog.lock";
/// Who holds the lock (see [`LockOwner`]).
pub const OWNER: &str = "catalog.lock.owner";

/// Who has the library open (as written in [`OWNER`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LockOwner {
    pub pid: u32,
    /// Computer name, if known.
    pub host: String,
    /// The program and its version (`Ember 0.3.0`, `lightcraft-cli 0.3.0`…).
    pub program: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl From<rightkit_fs::LockOwner> for LockOwner {
    fn from(o: rightkit_fs::LockOwner) -> Self {
        LockOwner { pid: o.pid, host: o.hostname, program: o.label }
    }
}

impl LockOwner {
    /// "Ember 0.3.0 (process 123 on studio-mac)".
    pub fn describe(&self) -> String {
        let program = if self.program.is_empty() { "another Ember" } else { self.program.as_str() };
        let host = if self.host.is_empty() {
            String::new()
        } else if self.host == host_name() {
            " on this computer".to_string()
        } else {
            format!(" on {}", self.host)
        };
        format!("{program} (process {}{host})", self.pid)
    }
}

/// This computer's name as `rightkit-fs` records it in an owner note (empty if unknown).
#[cfg(not(target_arch = "wasm32"))]
fn host_name() -> String {
    rightkit_fs::LockOwner::current("").hostname
}

#[cfg(target_arch = "wasm32")]
fn host_name() -> String {
    String::new()
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process has the library open.
    #[error("This library is already open in {}. Close it there first.", .0.as_ref().map(LockOwner::describe).unwrap_or_else(|| "another Ember program".into()))]
    InUse(Option<LockOwner>),
}

/// The exclusive lock on a library directory; released on drop (and by the OS if the process
/// dies).
#[derive(Debug)]
pub struct LibraryLock {
    dir: PathBuf,
    /// `None` when the file system can't lock (the library is open unlocked).
    #[cfg(not(target_arch = "wasm32"))]
    lock: Option<rightkit_fs::FileLock>,
}

impl LibraryLock {
    /// Lock `dir` (which must exist) for this process. Fails only when another process holds it.
    pub fn acquire(dir: &Path, program: &str) -> Result<LibraryLock, LockError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = dir.join(LOCK);
            let label = format!("{program} {}", env!("CARGO_PKG_VERSION"));
            let options = rightkit_fs::LockOptions::default().labeled(&label);
            match rightkit_fs::FileLock::acquire_kind(&path, rightkit_fs::LockKind::Flock, &options) {
                Ok(lock) => Ok(LibraryLock { dir: dir.to_path_buf(), lock: Some(lock) }),
                Err(rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. }) => {
                    Err(LockError::InUse(rightkit_fs::probe_owner(&path).map(LockOwner::from)))
                }
                Err(why) => {
                    // Not busy, but no exclusive lock with an owner note: the lock may have been taken and let go again when the note
                    // could not be written. Keep the protection anyway with a shared hold on the same lock file (an exclusive opener,
                    // which is what every Ember is, is refused by it); the note is only a hint.
                    match rightkit_fs::FileLock::acquire_shared_kind(&path, rightkit_fs::LockKind::Flock, &options) {
                        Ok(lock) => {
                            log::warn!("library {}: can't write {OWNER} ({why}); the library is locked without an owner note", dir.display());
                            Ok(LibraryLock { dir: dir.to_path_buf(), lock: Some(lock) })
                        }
                        Err(rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. }) => {
                            Err(LockError::InUse(rightkit_fs::probe_owner(&path).map(LockOwner::from)))
                        }
                        Err(_) => {
                            log::warn!(
                                "library {}: can't lock {LOCK} ({why}); opening it without protection against a second program",
                                dir.display()
                            );
                            Ok(LibraryLock { dir: dir.to_path_buf(), lock: None })
                        }
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = program;
            Ok(LibraryLock { dir: dir.to_path_buf() })
        }
    }

    /// The OS lock is held (`false`: the file system can't lock; opened unprotected).
    pub fn held(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.lock.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}
