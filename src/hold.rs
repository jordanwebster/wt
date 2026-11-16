//! Knowing when a tree is in use, through files in its `.wt`, since every
//! copy of wt on a machine (the CLI, and the library inside other tools)
//! meets only through files.
//!
//! - `.wt/holds.lock`: a process using the tree holds a shared `flock` on
//!   it; a stopper takes the exclusive lock, which the kernel grants at
//!   exactly the moment no shared lock remains. A holder that dies, however
//!   it dies, lets go with its process.
//! - `.wt/lifecycle.lock`: taken exclusively by whatever runs a hook, the
//!   `create` hook included, so hooks never overlap.
//! - `.wt/pinned`: a person ran `wt start`, so a stopper leaves the tree
//!   running.
//! - `.wt/owner`: who made the tree, an opaque string.
//! - `.wt/stopped`: the stop hook succeeded and nothing has started the tree
//!   since, so removal needn't stop it again. Written after a stop hook
//!   succeeds; removed before a start hook runs, after a stop hook that
//!   doesn't succeed, and whenever a hold is taken, since a holder may start
//!   things by hand. Unlike a "started" record it can't go stale the
//!   dangerous way: a reboot or a crash only makes it more true.
//!
//! Lock order: holds before lifecycle. Nothing blocks on the holds lock
//! while it holds the lifecycle lock, except a hook's own `wt exec`, which
//! `WT_IN_HOOK` keeps from holding.

use std::fs::{self, File, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result, io_at};
use crate::run::{self, Forward, Ran, RunHook};
use crate::{Hook, Tree, fsx, state};

pub(crate) const HOLDS: &str = "holds.lock";
pub(crate) const LIFECYCLE: &str = "lifecycle.lock";
const PINNED: &str = ".wt/pinned";
const OWNER: &str = ".wt/owner";
const STOPPED: &str = ".wt/stopped";

/// A shared hold on a tree, released when dropped (or when the process
/// ends). While any hold exists, [`Tree::stop_when_unheld`] waits and
/// [`crate::Wt::remove`] refuses.
#[derive(Debug)]
pub struct Hold {
    _file: File,
}

/// What [`Tree::stop_when_unheld`] did once nothing held the tree.
#[derive(Debug)]
pub enum Stopped {
    /// Ran the stop hook (or found none to run).
    Ran(Ran),
    /// Left it running: a person pinned it with `wt start`.
    Pinned,
    /// The tree was removed, or removed and made again, or forgotten with
    /// `wt rm --keep`, while it waited.
    Gone,
}

/// Whether anything uses a tree, as `wt ls` shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InUse {
    No,
    /// One or more holders.
    Held,
    /// A stop or a removal has the tree to itself.
    Stopping,
}

impl Tree {
    /// Holds the tree until the returned value is dropped. Waits while a
    /// stop runs, so a holder never sees a tree half stopped.
    ///
    /// The lock's file isn't inherited by child processes, so something
    /// started while holding doesn't keep the hold after this process ends.
    pub fn hold(&self) -> Result<Hold> {
        let file = run::open_lock(self, HOLDS)?;
        let path = self.holds_path();
        run::wait_for_lock(&file, true, None, &path)?;
        self.current(&file, &path)?;
        self.set_stopped(false)?;
        Ok(Hold { _file: file })
    }

    /// Like [`Tree::hold`], but `None` instead of waiting for a running
    /// stop.
    pub fn try_hold(&self) -> Result<Option<Hold>> {
        let file = run::open_lock(self, HOLDS)?;
        let path = self.holds_path();
        match file.try_lock_shared() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(io_at("locking", &path)(error)),
        }
        self.current(&file, &path)?;
        self.set_stopped(false)?;
        Ok(Some(Hold { _file: file }))
    }

    /// Blocks until nothing holds the tree, then runs its stop hook, keeping holders
    /// out until the stop hook has finished. Call it on a thread of its own.
    ///
    /// There is no "is the stop still wanted?" question to answer: a caller
    /// that holds the tree whenever it has a use for it can't be granted
    /// the stop while it does, so a grant means nobody wants the tree.
    ///
    /// Backs off from a pinned tree, and gives up on one removed (or
    /// removed and made again, or forgotten) while it waited. While another hook runs,
    /// such as a `create` that builds for minutes, it lets go of the tree,
    /// so new holders don't wait on the build, waits for that hook, and
    /// starts over.
    pub fn stop_when_unheld(&self, options: RunHook) -> Result<Stopped> {
        let path = self.holds_path();
        // Opened once: after waiting for another hook it is this file, the
        // tree's identity, that is locked again, so a tree removed and made
        // again meanwhile is seen as gone rather than stopped.
        let holds = match run::open_lock(self, HOLDS) {
            Ok(file) => file,
            Err(Error::NotFound(_)) => return Ok(Stopped::Gone),
            Err(error) => return Err(error),
        };
        loop {
            run::wait_for_lock(&holds, false, options.forward, &path)?;
            // Removed, perhaps made again under the same path, or forgotten
            // by `wt rm --keep` while this waited.
            let managed = state::read(&self.wt.home)?.tree_at(&self.root).is_some();
            if !is_current(&holds, &path) || !managed {
                return Ok(Stopped::Gone);
            }
            if self.pinned() {
                return Ok(Stopped::Pinned);
            }
            let lifecycle = run::open_lock(self, LIFECYCLE)?;
            match lifecycle.try_lock() {
                Ok(()) => {
                    let ran = self.run_locked(Hook::Stop, options, &lifecycle, Some(&holds));
                    // The hold before the lifecycle lock: a removal that finds
                    // the lifecycle lock free must find the hold free too, or
                    // it would take the end of a stop for a holder.
                    drop(holds);
                    drop(lifecycle);
                    let ran = ran?;
                    return Ok(Stopped::Ran(ran));
                }
                Err(TryLockError::WouldBlock) => {
                    holds
                        .unlock()
                        .map_err(io_at("unlocking", &self.holds_path()))?;
                    let lifecycle_path = self.root.join(".wt").join(LIFECYCLE);
                    run::wait_for_lock(&lifecycle, false, options.forward, &lifecycle_path)?;
                }
                Err(TryLockError::Error(error)) => {
                    return Err(io_at("locking", &self.root.join(".wt").join(LIFECYCLE))(
                        error,
                    ));
                }
            }
        }
    }

    /// Whether anything holds the tree right now.
    pub fn in_use(&self) -> InUse {
        // Opened, not created: looking must not write into every tree.
        let Ok(file) = File::open(self.holds_path()) else {
            return InUse::No;
        };
        match file.try_lock() {
            Ok(()) => InUse::No,
            Err(_) => match file.try_lock_shared() {
                Ok(()) => InUse::Held,
                Err(_) => InUse::Stopping,
            },
        }
    }

    /// Whether one of the tree's hooks is running now: something holds its
    /// lifecycle lock.
    pub fn hook_running(&self) -> bool {
        let Ok(file) = File::open(self.root.join(".wt").join(LIFECYCLE)) else {
            return false;
        };
        matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock))
    }

    /// Whether the tree's stop hook succeeded and nothing has started or
    /// held the tree since. Removal skips the stop hook then.
    pub fn stopped(&self) -> bool {
        self.root.join(STOPPED).exists()
    }

    pub(crate) fn set_stopped(&self, stopped: bool) -> Result<()> {
        let path = self.root.join(STOPPED);
        if stopped {
            fsx::write_atomic(&path, b"", None)
        } else {
            remove_if_there(&path)
        }
    }

    /// Whether a person pinned the tree running with `wt start`.
    pub fn pinned(&self) -> bool {
        self.root.join(PINNED).exists()
    }

    pub fn pin(&self) -> Result<()> {
        if !self.root.is_dir() {
            return Err(Error::NotFound(format!("{} was removed", self.id())));
        }
        fsx::write_atomic(&self.root.join(PINNED), b"", None)
    }

    pub fn unpin(&self) -> Result<()> {
        remove_if_there(&self.root.join(PINNED))
    }

    /// Who made the tree, as recorded by [`crate::NewTree::owner`].
    pub fn owner(&self) -> Option<String> {
        fs::read_to_string(self.root.join(OWNER)).ok()
    }

    /// Forgets who made the tree, so it is nobody's.
    pub fn disown(&self) -> Result<()> {
        remove_if_there(&self.root.join(OWNER))
    }

    pub(crate) fn set_owner(&self, owner: &str) -> Result<()> {
        fsx::write_atomic(&self.root.join(OWNER), owner.as_bytes(), None)
    }

    /// Takes the lifecycle lock, waiting out any hook that is running.
    pub(crate) fn lock_lifecycle(&self, forward: Option<Forward>) -> Result<File> {
        let file = run::open_lock(self, LIFECYCLE)?;
        let path = self.root.join(".wt").join(LIFECYCLE);
        run::wait_for_lock(&file, false, forward, &path)?;
        self.current(&file, &path)?;
        Ok(file)
    }

    /// Takes the exclusive hold for a removal, which never waits: `None`
    /// when something holds the tree.
    pub(crate) fn try_hold_exclusively(&self) -> Result<Option<File>> {
        let file = run::open_lock(self, HOLDS)?;
        // A few quick tries, because `wt ls` takes the lock for an instant
        // to see whether a tree is in use, and that is not a holder.
        for attempt in 0..5 {
            match file.try_lock() {
                Ok(()) => return Ok(Some(file)),
                Err(TryLockError::WouldBlock) if attempt < 4 => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(error)) => {
                    return Err(io_at("locking", &self.holds_path())(error));
                }
            }
        }
        unreachable!()
    }

    fn holds_path(&self) -> std::path::PathBuf {
        self.root.join(".wt").join(HOLDS)
    }

    /// Fails if `file` is no longer the lock at `path`: the tree was removed
    /// while this process waited, and perhaps made again.
    fn current(&self, file: &File, path: &Path) -> Result<()> {
        if is_current(file, path) {
            Ok(())
        } else {
            Err(Error::NotFound(format!("{} was removed", self.id())))
        }
    }
}

fn is_current(file: &File, path: &Path) -> bool {
    match (file.metadata(), fs::metadata(path)) {
        (Ok(open), Ok(there)) => open.dev() == there.dev() && open.ino() == there.ino(),
        _ => false,
    }
}

fn remove_if_there(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_at("removing", path)(error)),
    }
}
