use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wt_core::{CoreError, ExitClass};

use crate::{fsx, Result};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(u8)]
pub enum Level {
    /// Serialises one label's anchor refreshes (§11.10); held across the
    /// build, so it sits below every other level.
    Anchor = 0,
    Tree = 1,
    RepoGit = 2,
    Resource = 3,
    Named = 4,
    RegistryRmw = 5,
    StateRmw = 6,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Shared,
    Exclusive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    pub target: String,
    pub verb: String,
    pub since: String,
}

impl Holder {
    pub fn current(
        target: impl Into<String>,
        verb: impl Into<String>,
        since: impl Into<String>,
    ) -> Self {
        Self {
            pid: std::process::id(),
            target: target.into(),
            verb: verb.into(),
            since: since.into(),
        }
    }
}

#[derive(Debug)]
struct Guard {
    file: File,
    path: PathBuf,
    level: Level,
    traced: bool,
}

impl Guard {
    fn raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: the file owns a valid open descriptor; flock only changes
        // its advisory lock and does not transfer ownership.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
        if self.traced {
            trace_release(self.level);
        }
    }
}

#[derive(Debug)]
pub struct TreeToken(Guard);

impl TreeToken {
    pub fn raw_fd(&self) -> RawFd {
        self.0.raw_fd()
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }
}

#[derive(Debug)]
pub struct GitToken(Guard);

impl GitToken {
    pub fn raw_fd(&self) -> RawFd {
        self.0.raw_fd()
    }
}

#[derive(Debug)]
pub struct ResourceToken(Guard);

impl ResourceToken {
    pub fn raw_fd(&self) -> RawFd {
        self.0.raw_fd()
    }
}

#[derive(Debug)]
pub struct NamedToken(Guard);

impl NamedToken {
    pub fn raw_fd(&self) -> RawFd {
        self.0.raw_fd()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedSlotHolder {
    pub slot: u16,
    pub path: PathBuf,
    pub holder: Option<Holder>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedOccupancy {
    pub slots: u16,
    pub holders: Vec<NamedSlotHolder>,
}

#[derive(Debug)]
pub enum NamedAcquireError {
    Held(NamedOccupancy),
    Other(CoreError),
}

#[derive(Debug)]
pub struct RmwToken(Guard);

impl RmwToken {
    pub fn raw_fd(&self) -> RawFd {
        self.0.raw_fd()
    }
}

#[derive(Debug)]
pub struct DoorToken {
    guard: Option<Guard>,
    path: PathBuf,
}

impl DoorToken {
    pub fn raw_fd(&self) -> RawFd {
        self.guard
            .as_ref()
            .expect("door guard exists until drop")
            .raw_fd()
    }
}

impl Drop for DoorToken {
    fn drop(&mut self) {
        drop(self.guard.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
pub struct AnchorToken(Guard);

impl AnchorToken {
    pub fn raw_fd(&self) -> RawFd {
        self.0.raw_fd()
    }
}

/// Takes a label's anchor lock without waiting: a refresh already under way
/// is the reason not to start another (§11.10).
pub fn anchor(path: &Path, holder: &Holder) -> Result<AnchorToken> {
    acquire(path, Level::Anchor, Mode::Exclusive, holder, Duration::ZERO).map(AnchorToken)
}

/// Acquires a level-1 tree lock and returns its fd-owning token.
pub fn tree(path: &Path, mode: Mode, holder: &Holder, wait: Duration) -> Result<TreeToken> {
    let wait = if mode == Mode::Shared {
        Duration::ZERO
    } else {
        wait
    };
    acquire(path, Level::Tree, mode, holder, wait).map(TreeToken)
}

/// Acquires a level-2 repository git lock and returns its fd-owning token.
pub fn git(path: &Path, holder: &Holder, wait: Duration) -> Result<GitToken> {
    acquire(path, Level::RepoGit, Mode::Exclusive, holder, wait).map(GitToken)
}

/// Acquires a level-3 resource lock.
pub fn resource(path: &Path, holder: &Holder, wait: Duration) -> Result<ResourceToken> {
    acquire(path, Level::Resource, Mode::Exclusive, holder, wait).map(ResourceToken)
}

/// Acquires one slot of a level-4 named task lock.
pub fn named(
    dir: &Path,
    slots: u16,
    holder: &Holder,
    wait: Option<Duration>,
) -> std::result::Result<NamedToken, NamedAcquireError> {
    debug_assert!(slots > 0, "named locks always have at least one slot");
    let started = Instant::now();
    loop {
        for slot in 0..slots {
            let path = named_slot_path(dir, slot);
            match acquire_inner(
                &path,
                Level::Named,
                Mode::Exclusive,
                holder,
                Duration::ZERO,
                true,
            ) {
                Ok(guard) => return Ok(NamedToken(guard)),
                Err(error) if error.code.0 == "LOCK_HELD" => {}
                Err(error) => return Err(NamedAcquireError::Other(error)),
            }
        }

        if wait.is_some_and(|deadline| started.elapsed() >= deadline) {
            let occupancy = named_occupancy(dir, slots).map_err(NamedAcquireError::Other)?;
            if occupancy.holders.len() < usize::from(slots) {
                // A slot freed between the sweep and this read; re-attempt after
                // a short pause so a deadline overrun stays bounded instead of
                // spinning hot while holders churn.
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            return Err(NamedAcquireError::Held(occupancy));
        }

        let interval = wait.map_or(Duration::from_millis(100), |deadline| {
            Duration::from_millis(100).min(deadline.saturating_sub(started.elapsed()))
        });
        std::thread::sleep(interval);
    }
}

pub fn named_occupancy(dir: &Path, slots: u16) -> Result<NamedOccupancy> {
    let mut holders = Vec::new();
    for slot in 0..slots {
        let path = named_slot_path(dir, slot);
        if is_held(&path)? {
            holders.push(NamedSlotHolder {
                slot,
                holder: read_holder(&path)?,
                path,
            });
        }
    }
    Ok(NamedOccupancy { slots, holders })
}

fn named_slot_path(dir: &Path, slot: u16) -> PathBuf {
    dir.join(format!("{slot}.lock"))
}

/// Acquires the level-5 leaf registry RMW lock.
pub fn registry_rmw(path: &Path, holder: &Holder, wait: Duration) -> Result<RmwToken> {
    acquire(path, Level::RegistryRmw, Mode::Exclusive, holder, wait).map(RmwToken)
}

/// Acquires a level-6 leaf state RMW lock.
pub fn state_rmw(path: &Path, holder: &Holder, wait: Duration) -> Result<RmwToken> {
    acquire(path, Level::StateRmw, Mode::Exclusive, holder, wait).map(RmwToken)
}

/// Creates and exclusively holds a per-door holder file until the token drops.
pub fn door(path: &Path, holder: &Holder) -> Result<DoorToken> {
    let guard = acquire_inner(
        path,
        Level::Tree,
        Mode::Exclusive,
        holder,
        Duration::ZERO,
        false,
    )?;
    Ok(DoorToken {
        guard: Some(guard),
        path: path.to_path_buf(),
    })
}

/// Reads a holder record without taking or mutating the lock.
pub fn read_holder(path: &Path) -> Result<Option<Holder>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(lock_io("open holder record", path, error)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| lock_io("read holder record", path, error))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    // The flock is liveness truth; an unlocked read can race the in-place
    // holder rewrite, so a torn advisory record means only "holder unknown".
    Ok(serde_json::from_slice(&bytes).ok())
}

/// Uses a non-blocking exclusive flock as the sole liveness test for a lock file.
pub fn is_held(path: &Path) -> Result<bool> {
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(lock_io("open liveness lock", path, error)),
    };
    // SAFETY: file owns a valid descriptor for the duration of the probe.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        // SAFETY: this releases only the advisory lock just acquired above.
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
        Ok(false)
    } else {
        let error = std::io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Ok(true)
        } else {
            Err(lock_io("probe lock liveness", path, error))
        }
    }
}

fn acquire(
    path: &Path,
    level: Level,
    mode: Mode,
    holder: &Holder,
    wait: Duration,
) -> Result<Guard> {
    acquire_inner(path, level, mode, holder, wait, true)
}

fn acquire_inner(
    path: &Path,
    level: Level,
    mode: Mode,
    holder: &Holder,
    wait: Duration,
    traced: bool,
) -> Result<Guard> {
    let traced = traced && trace_enabled();
    let mut trace = TraceEntry::new(level, traced);
    if let Some(parent) = path.parent() {
        fsx::create_private_dir(parent)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|error| lock_io("open lock", path, error))?;
    let operation = match mode {
        Mode::Shared => libc::LOCK_SH,
        Mode::Exclusive => libc::LOCK_EX,
    } | libc::LOCK_NB;
    // Only a lock that actually blocked is worth timing; an uncontended
    // acquisition is a syscall, and recording every one would bury the waits
    // that explain a slow command.
    let timed = crate::trace::span("lock", format!("{level:?}").to_ascii_lowercase())
        .about(path.display().to_string());
    let mut waited = false;
    let started = Instant::now();
    loop {
        // SAFETY: file owns a valid descriptor and `operation` is a supported
        // flock mode with LOCK_NB.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if !error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            return Err(lock_io("acquire lock", path, error));
        }
        if started.elapsed() >= wait {
            return Err(timeout_error(level, mode, path));
        }
        waited = true;
        std::thread::sleep(Duration::from_millis(5));
    }
    if waited {
        timed.finish();
    }
    // Shared tree holders use distinct door files and must not concurrently
    // truncate the common tree lock's holder record (SPEC §4, §13.1).
    if mode == Mode::Exclusive || level != Level::Tree {
        let record = serde_json::to_vec(holder).map_err(|error| {
            CoreError::new(
                ExitClass::Internal,
                "SERIALIZE_FAILED",
                format!("could not serialize lock holder: {error}"),
                "report this wt bug",
            )
        })?;
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
            .and_then(|()| file.write_all(&record))
            .and_then(|()| file.sync_data())
            .map_err(|error| lock_io("write holder record", path, error))?;
    }
    trace_flock(path, level, mode)?;
    trace.disarm();
    Ok(Guard {
        file,
        path: path.to_path_buf(),
        level,
        traced,
    })
}

struct TraceEntry {
    level: Level,
    active: bool,
}

impl TraceEntry {
    fn new(level: Level, active: bool) -> Self {
        if active {
            trace_acquire(level);
        }
        Self { level, active }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TraceEntry {
    fn drop(&mut self) {
        if self.active {
            trace_release(self.level);
        }
    }
}

fn timeout_error(level: Level, mode: Mode, path: &Path) -> CoreError {
    let (class, code, remedy) = match (level, mode) {
        (Level::Tree, Mode::Shared) => (
            ExitClass::Conflict,
            "TREE_BUSY",
            "wait for the lifecycle operation to finish and retry",
        ),
        (Level::Tree, Mode::Exclusive) => (
            ExitClass::Conflict,
            "TREE_IN_USE",
            "wait for or stop the processes using the tree",
        ),
        (Level::RegistryRmw | Level::StateRmw, _) => (
            ExitClass::Timeout,
            "LOCK_TIMEOUT",
            "retry after the short state update finishes",
        ),
        _ => (
            ExitClass::Conflict,
            "LOCK_HELD",
            "wait for the named operation or choose a longer wait",
        ),
    };
    CoreError::new(
        class,
        code,
        format!("timed out waiting for {}", path.display()),
        remedy,
    )
}

fn lock_io(action: &str, path: &Path, error: std::io::Error) -> CoreError {
    CoreError::new(
        ExitClass::Internal,
        "LOCK_IO_FAILED",
        format!("{action} {}: {error}", path.display()),
        "retry the operation and inspect lock-directory permissions if it repeats",
    )
}

thread_local! {
    static HELD_LEVELS: RefCell<Vec<Level>> = const { RefCell::new(Vec::new()) };
}

fn trace_enabled() -> bool {
    cfg!(debug_assertions)
        || cfg!(feature = "lock-trace")
        || std::env::var_os("WT_LOCK_TRACE").is_some()
}

fn trace_flock(path: &Path, level: Level, mode: Mode) -> Result<()> {
    let Some(trace_path) = std::env::var_os("WT_LOCK_TRACE_FILE") else {
        return Ok(());
    };
    let mut trace = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&trace_path)
        .map_err(|error| lock_io("open lock trace", Path::new(&trace_path), error))?;
    let entry = serde_json::json!({
        "level": level,
        "mode": match mode {
            Mode::Shared => "shared",
            Mode::Exclusive => "exclusive",
        },
        "path": path,
    });
    serde_json::to_writer(&mut trace, &entry).map_err(|error| {
        CoreError::new(
            ExitClass::Internal,
            "LOCK_TRACE_FAILED",
            format!("could not encode lock trace: {error}"),
            "disable WT_LOCK_TRACE_FILE and retry",
        )
    })?;
    trace
        .write_all(b"\n")
        .map_err(|error| lock_io("write lock trace", Path::new(&trace_path), error))
}

fn trace_acquire(level: Level) {
    HELD_LEVELS.with(|held| {
        let held = held.borrow();
        // Executors intentionally hold at most one resource lock at a time;
        // equal levels therefore indicate a real lock-plan violation.
        assert!(
            held.last().is_none_or(|current| *current < level),
            "lock order violation: attempted {level:?} while holding {held:?}"
        );
    });
    HELD_LEVELS.with(|held| held.borrow_mut().push(level));
}

fn trace_release(level: Level) {
    HELD_LEVELS.with(|held| {
        let popped = held.borrow_mut().pop();
        assert_eq!(
            popped,
            Some(level),
            "locks must be released in reverse order"
        );
    });
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn holder() -> Holder {
        Holder::current("repo/tree", "test", "2026-01-01T00:00:00Z")
    }

    #[test]
    fn six_levels_acquire_in_order_and_out_of_order_panics() {
        let dir = tempdir().unwrap();
        let tree_token = tree(
            &dir.path().join("1"),
            Mode::Shared,
            &holder(),
            Duration::ZERO,
        )
        .unwrap();
        let git = git(&dir.path().join("2"), &holder(), Duration::ZERO).unwrap();
        let resource = resource(&dir.path().join("3"), &holder(), Duration::ZERO).unwrap();
        let named_token = named(&dir.path().join("4"), 1, &holder(), Some(Duration::ZERO)).unwrap();
        let registry = registry_rmw(&dir.path().join("5"), &holder(), Duration::ZERO).unwrap();
        let state = state_rmw(&dir.path().join("6"), &holder(), Duration::ZERO).unwrap();
        drop(state);
        drop(registry);
        drop(named_token);
        drop(resource);
        drop(git);
        drop(tree_token);

        let high = named(&dir.path().join("high"), 1, &holder(), Some(Duration::ZERO)).unwrap();
        let result = std::panic::catch_unwind(|| {
            let _ = tree(
                &dir.path().join("low"),
                Mode::Shared,
                &holder(),
                Duration::ZERO,
            );
        });
        assert!(result.is_err());
        drop(high);
    }

    #[test]
    fn holder_record_and_try_flock_are_liveness_truth() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tree.lock");
        let token = tree(&path, Mode::Exclusive, &holder(), Duration::ZERO).unwrap();
        assert!(is_held(&path).unwrap());
        assert_eq!(read_holder(&path).unwrap(), Some(holder()));
        drop(token);
        assert!(!is_held(&path).unwrap());
    }

    #[test]
    fn torn_advisory_holder_record_is_reported_as_unknown() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tree.lock");
        std::fs::write(&path, b"{\"pid\":").unwrap();
        assert_eq!(read_holder(&path).unwrap(), None);
        assert!(!is_held(&path).unwrap());
    }

    #[test]
    fn shared_tree_and_door_record_are_one_level_and_can_coexist() {
        let dir = tempdir().unwrap();
        let tree_path = dir.path().join("tree.lock");
        let door_path = dir.path().join("tree.doors/123.lock");
        let tree_token = tree(&tree_path, Mode::Shared, &holder(), Duration::ZERO).unwrap();
        let door_token = door(&door_path, &holder()).unwrap();
        assert_eq!(read_holder(&door_path).unwrap(), Some(holder()));
        assert!(is_held(&door_path).unwrap());
        drop(door_token);
        assert!(!door_path.exists());
        drop(tree_token);
    }

    #[test]
    fn section_13_3_named_capacity_fills_slots_and_waits_for_the_first_free_one() {
        use std::sync::mpsc;

        let dir = tempdir().unwrap();
        let lock_dir = dir.path().join("named/serial");
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_dir = lock_dir.clone();
        let first_ready = ready_tx.clone();
        let first = std::thread::spawn(move || {
            let token = named(
                &first_dir,
                2,
                &Holder::current("repo/first", "run", "first"),
                Some(Duration::ZERO),
            )
            .unwrap();
            first_ready.send(()).unwrap();
            release_first_rx.recv().unwrap();
            drop(token);
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (release_second_tx, release_second_rx) = mpsc::channel();
        let second_dir = lock_dir.clone();
        let second = std::thread::spawn(move || {
            let token = named(
                &second_dir,
                2,
                &Holder::current("repo/second", "run", "second"),
                Some(Duration::ZERO),
            )
            .unwrap();
            ready_tx.send(()).unwrap();
            release_second_rx.recv().unwrap();
            drop(token);
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let error = named(
            &lock_dir,
            2,
            &Holder::current("repo/third", "run", "third"),
            Some(Duration::ZERO),
        )
        .unwrap_err();
        let NamedAcquireError::Held(occupancy) = error else {
            panic!("full named lock must report occupancy");
        };
        assert_eq!(occupancy.slots, 2);
        assert_eq!(
            occupancy
                .holders
                .iter()
                .map(|holder| holder.slot)
                .collect::<Vec<_>>(),
            [0, 1]
        );

        let (waiter_ready_tx, waiter_ready_rx) = mpsc::channel();
        let (release_waiter_tx, release_waiter_rx) = mpsc::channel();
        let waiter_dir = lock_dir.clone();
        let waiter = std::thread::spawn(move || {
            let token = named(
                &waiter_dir,
                2,
                &Holder::current("repo/waiter", "run", "waiter"),
                Some(Duration::from_secs(1)),
            )
            .unwrap();
            waiter_ready_tx.send(()).unwrap();
            release_waiter_rx.recv().unwrap();
            drop(token);
        });
        release_first_tx.send(()).unwrap();
        waiter_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let occupancy = named_occupancy(&lock_dir, 2).unwrap();
        assert_eq!(occupancy.holders.len(), 2);
        assert!(occupancy.holders.iter().any(|slot| slot
            .holder
            .as_ref()
            .is_some_and(|holder| holder.target == "repo/waiter")));

        release_second_tx.send(()).unwrap();
        release_waiter_tx.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn section_4_named_slot_ignores_a_held_flat_leftover() {
        let dir = tempdir().unwrap();
        let flat = dir.path().join("named/serial.lock");
        let flat_token = tree(&flat, Mode::Exclusive, &holder(), Duration::ZERO).unwrap();
        let slot_dir = dir.path().join("named/serial");
        let named_token = named(&slot_dir, 1, &holder(), Some(Duration::ZERO)).unwrap();
        assert!(slot_dir.join("0.lock").exists());
        drop(named_token);
        drop(flat_token);
    }

    #[test]
    fn deadline_returns_the_class_for_the_requested_level() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tree.lock");
        let held = tree(&path, Mode::Exclusive, &holder(), Duration::ZERO).unwrap();
        let contender_path = path.clone();
        let error = std::thread::spawn(move || {
            tree(
                &contender_path,
                Mode::Shared,
                &holder(),
                Duration::from_millis(10),
            )
            .unwrap_err()
        })
        .join()
        .unwrap();
        assert_eq!(error.code.0, "TREE_BUSY");
        drop(held);
    }
}
