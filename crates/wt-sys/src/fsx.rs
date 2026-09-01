use std::ffi::{CStr, CString};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Serialize;
use wt_core::adapters::DirSnapshot;
use wt_core::model::{scope_enc, RelPath};
use wt_core::render::Observed;
use wt_core::{CoreError, ExitClass};

use crate::Result;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Appends text to a file, keeping a backup of what was there.
///
/// The path is used as given: a caller that means to edit through a symlink
/// resolves it first and names the real file in its consent, because an
/// unresolved append silently edits whatever the link points at (A76).
/// Returns the backup's path when one was made; a file that did not exist has
/// nothing to back up.
pub fn append_with_backup(path: &Path, text: &str) -> Result<Option<PathBuf>> {
    let existing = read_string(path)?;
    // An rc file is ordinarily world-readable; an append that quietly made it
    // private would be an edit nobody agreed to.
    let mode = std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o777)
        .unwrap_or(0o600);
    let backup = match &existing {
        Some(contents) => {
            let backup = path.with_extension(match path.extension() {
                Some(extension) => format!("{}.wt-backup", extension.to_string_lossy()),
                None => "wt-backup".to_owned(),
            });
            write_store(&backup, contents.as_bytes())?;
            Some(backup)
        }
        None => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    CoreError::new(
                        ExitClass::Internal,
                        "WRITE_FAILED",
                        format!("could not create {}: {error}", parent.display()),
                        "check the directory's permissions",
                    )
                })?;
            }
            None
        }
    };
    let mut merged = existing.unwrap_or_default();
    if !merged.is_empty() && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged.push_str(text);
    write_atomic_mode(path, merged.as_bytes(), mode)?;
    Ok(backup)
}

/// Seconds since the epoch, for comparing against a file's mtime.
pub fn epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
static STORE_FAIL_AFTER: AtomicU64 = AtomicU64::new(u64::MAX);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CopyReport {
    pub files: Vec<PathBuf>,
    pub symlinks: Vec<PathBuf>,
    pub directories: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExcludeWrite {
    pub changed: bool,
    pub repaired: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathKind {
    Missing,
    File,
    Directory,
    Symlink,
    Other,
}

/// Returns the no-follow kind needed by orchestration decisions.
pub fn path_kind(path: &Path) -> Result<PathKind> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(PathKind::Symlink),
        Ok(metadata) if metadata.is_file() => Ok(PathKind::File),
        Ok(metadata) if metadata.is_dir() => Ok(PathKind::Directory),
        Ok(_) => Ok(PathKind::Other),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(PathKind::Missing),
        Err(error) => Err(fs_error("inspect path", path)(error)),
    }
}

/// Canonicalises an observed path without performing any mutation.
pub fn canonicalize(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(fs_error("canonicalize path", path))
}

pub fn rename_path(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).map_err(fs_error("rename path", from))
}

pub fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(fs_error("read file", path)(error)),
    }
}

pub fn read_string(path: &Path) -> Result<Option<String>> {
    let Some(bytes) = read_bytes(path)? else {
        return Ok(None);
    };
    String::from_utf8(bytes).map(Some).map_err(|error| {
        CoreError::new(
            ExitClass::State,
            "FILE_INVALID_UTF8",
            format!("{} is not UTF-8: {error}", path.display()),
            "replace the file with valid UTF-8",
        )
    })
}

/// Appends one budget observation. This is inert unless the contract harness
/// explicitly supplies `WT_BUDGET_TRACE`.
pub fn trace_budget(kind: &str, path: Option<&Path>) -> Result<()> {
    let Some(trace_path) = std::env::var_os("WT_BUDGET_TRACE") else {
        return Ok(());
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(Path::new(&trace_path))
        .map_err(fs_error("open budget trace", Path::new(&trace_path)))?;
    let record = serde_json::json!({
        "kind": kind,
        "path": path.map(|value| value.to_string_lossy().into_owned()),
    });
    serde_json::to_writer(&mut file, &record).map_err(|error| {
        CoreError::new(
            ExitClass::Internal,
            "TRACE_FAILED",
            format!("could not encode budget trace: {error}"),
            "disable WT_BUDGET_TRACE and retry",
        )
    })?;
    file.write_all(b"\n")
        .map_err(fs_error("write budget trace", Path::new(&trace_path)))
}

/// Computes apparent disk usage without following symlinks.
pub fn disk_kb(path: &Path) -> Result<u64> {
    fn bytes(path: &Path) -> Result<u64> {
        let metadata =
            std::fs::symlink_metadata(path).map_err(fs_error("inspect disk usage", path))?;
        if metadata.file_type().is_symlink() || metadata.is_file() {
            return Ok(metadata.len());
        }
        if !metadata.is_dir() {
            return Ok(0);
        }
        let mut total = metadata.len();
        for entry in std::fs::read_dir(path).map_err(fs_error("read disk usage", path))? {
            let entry = entry.map_err(fs_error("read disk usage entry", path))?;
            total = total.saturating_add(bytes(&entry.path())?);
        }
        Ok(total)
    }

    bytes(path).map(|value| value.saturating_add(1023) / 1024)
}

pub fn read_dir_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(fs_error("read directory", path)(error)),
    };
    let mut paths = entries
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(fs_error("read directory entry", path))
        })
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

pub fn read_link(path: &Path) -> Result<Option<PathBuf>> {
    match std::fs::read_link(path) {
        Ok(target) => Ok(Some(target)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(fs_error("read symlink", path)(error)),
    }
}

/// Atomically installs one owned symlink without ever following the previous
/// final component. A non-symlink occupant is left untouched.
pub fn replace_symlink(path: &Path, target: &Path) -> Result<bool> {
    if read_link(path)?.as_deref() == Some(target) {
        return Ok(false);
    }
    match path_kind(path)? {
        PathKind::Missing | PathKind::Symlink => {}
        _ => {
            return Err(CoreError::new(
                ExitClass::State,
                "SHIM_CONFLICT",
                format!("refusing to replace non-symlink `{}`", path.display()),
                "remove the conflicting path and retry",
            ))
        }
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    create_private_dir(parent)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".wt-shim-{}-{sequence}", std::process::id()));
    symlink(target, &temporary).map_err(fs_error("create shim symlink", &temporary))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(fs_error("install shim symlink", path)(error));
    }
    Ok(true)
}

pub fn remove_empty_dir(path: &Path) -> Result<bool> {
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(fs_error("remove empty directory", path)(error)),
    }
}

/// Reports whether the name at `path` is something `exec` would run.
///
/// Symlinks are resolved here, unlike everywhere else in this module: PATH
/// lookup, the `which` verb and the teardown fall-through inventory all model
/// what the kernel would execute under this name, and a great many installed
/// tools are symlinks (`cargo` -> `rustup`, npm, most of Homebrew). Judging the
/// link itself would report them absent, and would silently weaken the
/// inventory that stops a teardown recipe reaching an installed binary. A
/// dangling or looping link resolves to nothing and is not executable.
pub fn is_executable_file(path: &Path) -> Result<bool> {
    let link = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(fs_error("inspect executable", path)(error)),
    };
    if link.is_file() {
        return Ok(link.permissions().mode() & 0o111 != 0);
    }
    if !link.is_symlink() {
        return Ok(false);
    }
    Ok(std::fs::metadata(path)
        .map(|target| target.is_file() && target.permissions().mode() & 0o111 != 0)
        .unwrap_or(false))
}

/// Removes one exact observed path without following a symlink at the root.
pub fn remove_path(path: &Path) -> Result<bool> {
    match path_kind(path)? {
        PathKind::Missing => Ok(false),
        PathKind::Directory => std::fs::remove_dir_all(path)
            .map(|()| true)
            .map_err(fs_error("remove directory", path)),
        _ => std::fs::remove_file(path)
            .map(|()| true)
            .map_err(fs_error("remove file", path)),
    }
}

/// Generates the random 128-bit lowercase identity required for an incarnation.
pub fn random_tree_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| {
            CoreError::new(
                ExitClass::Internal,
                "RANDOM_FAILED",
                format!("could not obtain a tree identity: {error}"),
                "retry; if the error persists, inspect /dev/urandom",
            )
        })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Produces a stable, sortable wall-clock field for persisted provenance.
pub fn timestamp() -> Result<String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            CoreError::new(
                ExitClass::Internal,
                "CLOCK_FAILED",
                format!("system clock is before the Unix epoch: {error}"),
                "correct the system clock and retry",
            )
        })?;
    let seconds = duration.as_secs() as libc::time_t;
    // SAFETY: gmtime_r initializes this local value and retains no pointer.
    let mut broken_down = unsafe { std::mem::zeroed::<libc::tm>() };
    // SAFETY: both pointers remain valid for the duration of the call.
    if unsafe { libc::gmtime_r(&seconds, &mut broken_down) }.is_null() {
        return Err(CoreError::new(
            ExitClass::Internal,
            "CLOCK_FAILED",
            "could not convert the system clock to UTC",
            "correct the system clock and retry",
        ));
    }
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        broken_down.tm_year + 1900,
        broken_down.tm_mon + 1,
        broken_down.tm_mday,
        broken_down.tm_hour,
        broken_down.tm_min,
        broken_down.tm_sec,
        duration.subsec_nanos(),
    ))
}

/// Creates an owned directory tree with an explicit mode independent of umask.
pub fn create_private_dir(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .map_err(fs_error("create private directory", path))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(fs_error("set private directory mode", path))
}

/// Implements the durable same-directory tmp + fsync + rename + dir-fsync store protocol.
pub fn write_store(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_mode(path, bytes, 0o600)
}

/// Serialises and durably writes one JSON store document.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        CoreError::new(
            ExitClass::Internal,
            "SERIALIZE_FAILED",
            format!("could not serialize {}: {error}", path.display()),
            "report this wt bug",
        )
    })?;
    write_store(path, &bytes)
}

/// Reads and parses one JSON store document; a missing file is `None`.
pub fn read_json<T: DeserializeOwned>(path: &Path, corrupt_code: &str) -> Result<Option<T>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(fs_error("read store file", path)(error)),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        CoreError::new(
            ExitClass::State,
            corrupt_code,
            format!("{} is corrupt: {error}", path.display()),
            format!(
                "delete `{}` and re-run `wt register` or `wt adopt` for the affected checkout",
                path.display()
            ),
        )
    })
}

/// Reads a contained file without following the root, a parent, or the target symlink.
pub fn read_nofollow(root: &Path, relative: &RelPath) -> Result<Vec<u8>> {
    let (parent, name) = open_parent(root, relative, false)?;
    let fd = openat(
        parent.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    let mut file = File::from(fd);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(fs_error(
        "read no-follow file",
        &root.join(relative.as_str()),
    ))?;
    Ok(bytes)
}

/// Observes a render target without following a parent or final symlink.
pub fn observe_nofollow(root: &Path, relative: &RelPath, tracked: bool) -> Result<Observed> {
    if tracked {
        return Ok(Observed::Tracked);
    }
    let (parent, name) = match open_parent(root, relative, false) {
        Ok(value) => value,
        Err(error) if error.code.0 == "PATH_MISSING" => return Ok(Observed::Absent),
        Err(error) => return Err(error),
    };
    let Some(stat) = statat(parent.as_raw_fd(), &name)? else {
        return Ok(Observed::Absent);
    };
    match stat.st_mode & libc::S_IFMT {
        libc::S_IFLNK => Ok(Observed::Symlink),
        libc::S_IFREG => read_nofollow(root, relative).map(Observed::Regular),
        _ => Ok(Observed::Other),
    }
}

/// Atomically writes a contained target while refusing symlinked path components.
pub fn write_nofollow(root: &Path, relative: &RelPath, bytes: &[u8], mode: u32) -> Result<()> {
    let (parent, name) = open_parent(root, relative, true)?;
    reject_symlink_target(parent.as_raw_fd(), &name)?;
    write_at(parent.as_raw_fd(), &name, bytes, mode)
}

/// Removes one contained non-directory entry without following symlinks.
pub fn remove_nofollow(root: &Path, relative: &RelPath) -> Result<bool> {
    let (parent, name) = match open_parent(root, relative, false) {
        Ok(value) => value,
        Err(error) if error.code.0 == "PATH_MISSING" => return Ok(false),
        Err(error) => return Err(error),
    };
    if statat(parent.as_raw_fd(), &name)?.is_none() {
        return Ok(false);
    }
    // SAFETY: `parent` is an owned directory fd and `name` is a NUL-terminated
    // single component. unlinkat does not follow a final symlink.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } < 0 {
        return Err(io_context("remove contained entry")(
            std::io::Error::last_os_error(),
        ));
    }
    sync_dir_fd(parent.as_raw_fd())?;
    Ok(true)
}

/// Recursively removes one contained directory without following symlinks.
pub fn remove_dir_all_nofollow(root: &Path, relative: &RelPath) -> Result<bool> {
    let (parent, name) = match open_parent(root, relative, false) {
        Ok(value) => value,
        Err(error) if error.code.0 == "PATH_MISSING" => return Ok(false),
        Err(error) => return Err(error),
    };
    let Some(stat) = statat(parent.as_raw_fd(), &name)? else {
        return Ok(false);
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return remove_nofollow(root, relative);
    }
    let directory = openat(
        parent.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    remove_dir_contents(directory.as_raw_fd())?;
    // SAFETY: the parent fd and final component are owned/validated above;
    // AT_REMOVEDIR removes the directory entry without following it.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
        return Err(io_context("remove contained directory")(
            std::io::Error::last_os_error(),
        ));
    }
    sync_dir_fd(parent.as_raw_fd())?;
    Ok(true)
}

/// Copies a contained file tree byte-for-byte while recreating symlinks.
pub fn copy_contained(
    source_root: &Path,
    destination_root: &Path,
    relative: &RelPath,
) -> Result<CopyReport> {
    reject_symlink_components(
        source_root,
        Path::new(relative.as_str())
            .parent()
            .unwrap_or(Path::new("")),
    )?;
    reject_symlink_components(
        destination_root,
        Path::new(relative.as_str())
            .parent()
            .unwrap_or(Path::new("")),
    )?;
    let source = source_root.join(relative.as_str());
    let mut report = CopyReport::default();
    copy_entry(
        &source,
        destination_root,
        Path::new(relative.as_str()),
        &mut report,
    )?;
    Ok(report)
}

/// Applies the managed exclude format while preserving all bytes outside its block.
pub fn splice_exclude(path: &Path, managed: &str) -> Result<ExcludeWrite> {
    let mode = std::fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map_or(0o644, |metadata| metadata.permissions().mode() & 0o777);
    let existing = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|error| {
            CoreError::new(
                ExitClass::State,
                "EXCLUDE_INVALID",
                format!("{} is not UTF-8: {error}", path.display()),
                "repair the git exclude file and retry",
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(fs_error("read git exclude", path)(error)),
    };
    let splice = wt_core::exclude::splice(&existing, managed);
    if splice.text == existing {
        return Ok(ExcludeWrite {
            changed: false,
            repaired: splice.repaired,
        });
    }
    write_atomic_mode(path, splice.text.as_bytes(), mode)?;
    Ok(ExcludeWrite {
        changed: true,
        repaired: splice.repaired,
    })
}

/// Removes only wt's managed exclude block, preserving all surrounding bytes.
pub fn remove_exclude(path: &Path) -> Result<ExcludeWrite> {
    let mode = std::fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map_or(0o644, |metadata| metadata.permissions().mode() & 0o777);
    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(fs_error("read exclude file", path)(error)),
    };
    let splice = wt_core::exclude::remove(&existing);
    let changed = splice.text != existing;
    if changed {
        write_atomic_mode(path, splice.text.as_bytes(), mode)?;
    }
    Ok(ExcludeWrite {
        changed,
        repaired: splice.repaired,
    })
}

/// Before a new log is created, retains at most `keep - 1` existing logs for its task.
pub fn retain_logs(log_dir: &Path, scope: &str, task: &str, keep: u16) -> Result<Vec<PathBuf>> {
    let encoded_scope = scope_enc(&RelPath::new(scope)?);
    let prefix = format!("{encoded_scope}-{task}-");
    let mut matches = match std::fs::read_dir(log_dir) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter_map(|path| {
                let timestamp = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| parse_log_timestamp(name, &prefix))
                    .map(str::to_owned)?;
                Some((timestamp, path))
            })
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(fs_error("read log directory", log_dir)(error)),
    };
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    let existing_to_keep = usize::from(keep.saturating_sub(1));
    let remove_count = matches.len().saturating_sub(existing_to_keep);
    let removed = matches
        .into_iter()
        .take(remove_count)
        .map(|(_, path)| path)
        .collect::<Vec<_>>();
    for path in &removed {
        std::fs::remove_file(path).map_err(fs_error("remove old task log", path))?;
    }
    Ok(removed)
}

fn parse_log_timestamp<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    let timestamp = name.strip_prefix(prefix)?.strip_suffix(".log")?;
    valid_utc_log_timestamp(timestamp).then_some(timestamp)
}

fn valid_utc_log_timestamp(timestamp: &str) -> bool {
    if timestamp.len() == 8 && timestamp.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    let Some(body) = timestamp.strip_suffix('Z') else {
        return false;
    };
    if let Some((seconds, nanos)) = body.split_once('.') {
        if (9..=12).contains(&seconds.len())
            && seconds.bytes().all(|byte| byte.is_ascii_digit())
            && nanos.len() == 9
            && nanos.bytes().all(|byte| byte.is_ascii_digit())
        {
            return true;
        }
    }
    let (date, time) = match body.split_once('T') {
        Some(parts) => parts,
        None => return false,
    };
    let valid_date = (date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()))
        || (date.len() == 10
            && date.as_bytes()[4] == b'-'
            && date.as_bytes()[7] == b'-'
            && date
                .bytes()
                .enumerate()
                .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit()));
    let (clock, fraction_valid) = time
        .split_once('.')
        .map_or((time, true), |(clock, fraction)| {
            (
                clock,
                !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit()),
            )
        });
    let valid_time = (clock.len() == 6 && clock.bytes().all(|byte| byte.is_ascii_digit()))
        || (clock.len() == 8
            && clock.as_bytes()[2] == b':'
            && clock.as_bytes()[5] == b':'
            && clock
                .bytes()
                .enumerate()
                .all(|(index, byte)| matches!(index, 2 | 5) || byte.is_ascii_digit()));
    valid_date && valid_time && fraction_valid
}

/// Captures names and selected small text files for pure adapter detection.
pub fn capture_dir_snapshot(
    root: &Path,
    dir: &RelPath,
    content_names: &[String],
) -> Result<DirSnapshot> {
    let path = if dir.as_str() == "." {
        root.to_path_buf()
    } else {
        root.join(dir.as_str())
    };
    let mut snapshot = DirSnapshot {
        dir: dir.as_str().to_owned(),
        ..DirSnapshot::default()
    };
    for entry in std::fs::read_dir(&path).map_err(fs_error("scan adapter directory", &path))? {
        let entry = entry.map_err(fs_error("read adapter directory entry", &path))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        snapshot.names.insert(name.clone());
        if content_names.iter().any(|wanted| wanted == &name) {
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(fs_error("inspect adapter input", &entry.path()))?;
            if metadata.is_file() && metadata.len() <= 1024 * 1024 {
                let content = std::fs::read_to_string(entry.path())
                    .map_err(fs_error("read adapter input", &entry.path()))?;
                snapshot.contents.insert(name, content);
            }
        }
    }
    Ok(snapshot)
}

fn write_atomic_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        create_private_dir(parent)?;
    }
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".wt-tmp-{}-{sequence}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(mode);
    let mut file = options
        .open(&tmp)
        .map_err(fs_error("create store temporary", &tmp))?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(fs_error("set store temporary mode", &tmp))?;
    store_boundary(0)?;
    file.write_all(bytes)
        .map_err(fs_error("write store temporary", &tmp))?;
    store_boundary(1)?;
    file.sync_all()
        .map_err(fs_error("fsync store temporary", &tmp))?;
    store_boundary(2)?;
    std::fs::rename(&tmp, path).map_err(fs_error("rename store temporary", path))?;
    store_boundary(3)?;
    let directory = File::open(parent).map_err(fs_error("open store directory", parent))?;
    store_boundary(4)?;
    directory
        .sync_all()
        .map_err(fs_error("fsync store directory", parent))?;
    store_boundary(5)
}

#[cfg(test)]
fn store_boundary(index: u64) -> Result<()> {
    if STORE_FAIL_AFTER.load(Ordering::Relaxed) == index {
        return Err(CoreError::new(
            ExitClass::Internal,
            "STORE_TEST_INTERRUPT",
            "store protocol interrupted by its unit-test boundary",
            "retry the store write",
        ));
    }
    Ok(())
}

#[cfg(not(test))]
fn store_boundary(_index: u64) -> Result<()> {
    Ok(())
}

fn open_parent(root: &Path, relative: &RelPath, create: bool) -> Result<(OwnedFd, CString)> {
    open_parent_path(root, Path::new(relative.as_str()), create)
}

fn open_parent_path(root: &Path, relative: &Path, create: bool) -> Result<(OwnedFd, CString)> {
    let root_name = CString::new(root.as_os_str().as_bytes()).map_err(nul_error)?;
    // SAFETY: `root_name` is NUL-terminated; open returns a new descriptor or
    // a negative error and does not borrow the string afterward.
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(nofollow_error(root, std::io::Error::last_os_error()));
    }
    // SAFETY: the non-negative descriptor returned by open is newly owned.
    let mut current = unsafe { OwnedFd::from_raw_fd(root_fd) };
    let mut parts = relative.components().collect::<Vec<_>>();
    let name = parts.pop().expect("contained relative path is non-empty");
    for component in parts {
        let component = CString::new(component.as_os_str().as_bytes()).map_err(nul_error)?;
        let next = match openat(
            current.as_raw_fd(),
            &component,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        ) {
            Ok(fd) => fd,
            Err(error) if create && error.code.0 == "PATH_MISSING" => {
                // SAFETY: current is an owned directory fd and component is a
                // single NUL-terminated relative component.
                let result =
                    unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o700) };
                if result < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(nofollow_error(root, std::io::Error::last_os_error()));
                }
                openat(
                    current.as_raw_fd(),
                    &component,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )?
            }
            Err(error) => return Err(error),
        };
        current = next;
    }
    Ok((
        current,
        CString::new(name.as_os_str().as_bytes()).map_err(nul_error)?,
    ))
}

fn openat(dir: i32, name: &CString, flags: i32, mode: u32) -> Result<OwnedFd> {
    // SAFETY: `dir` is an open directory fd, `name` is NUL-terminated, and a
    // successful openat returns a fresh descriptor owned by this function.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags, mode) };
    if fd >= 0 {
        // SAFETY: `fd` is non-negative and freshly returned by openat.
        return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Err(CoreError::new(
            ExitClass::NotFound,
            "PATH_MISSING",
            format!("path component `{}` does not exist", name.to_string_lossy()),
            "create the source path or choose an existing contained path",
        ))
    } else {
        Err(nofollow_error(
            Path::new(&name.to_string_lossy().into_owned()),
            error,
        ))
    }
}

fn reject_symlink_target(dir: i32, name: &CString) -> Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the stat buffer is writable, `dir` is an open directory fd, and
    // AT_SYMLINK_NOFOLLOW prevents dereferencing the final component.
    let result = unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(nofollow_error(
            Path::new(&name.to_string_lossy().into_owned()),
            error,
        ));
    }
    // SAFETY: fstatat succeeded and initialized the stat buffer.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT == libc::S_IFLNK {
        return Err(CoreError::new(
            ExitClass::State,
            "SYMLINK_REFUSED",
            format!("refusing symlink target `{}`", name.to_string_lossy()),
            "replace the symlink with an owned regular file path",
        ));
    }
    Ok(())
}

fn statat(dir: i32, name: &CString) -> Result<Option<libc::stat>> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the stat buffer is writable and AT_SYMLINK_NOFOLLOW ensures the
    // final component is observed rather than followed.
    let result = unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: successful fstatat initialized the entire stat structure.
        return Ok(Some(unsafe { stat.assume_init() }));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(io_context("inspect contained entry")(error))
    }
}

fn remove_dir_contents(dir: i32) -> Result<()> {
    // SAFETY: dup creates a separately owned descriptor for fdopendir, which
    // takes ownership and closes it when closedir is called.
    let duplicate = unsafe { libc::dup(dir) };
    if duplicate < 0 {
        return Err(io_context("duplicate contained directory")(
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: `duplicate` is a fresh directory descriptor transferred to DIR.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        // SAFETY: fdopendir failed, so ownership of duplicate was not taken.
        unsafe { libc::close(duplicate) };
        return Err(io_context("open contained directory stream")(
            std::io::Error::last_os_error(),
        ));
    }
    let result = (|| {
        loop {
            // SAFETY: stream remains valid until the single closedir below.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                break;
            }
            // SAFETY: readdir returned a valid dirent whose d_name is a
            // NUL-terminated array valid until the next readdir call.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            let name = CString::new(name.to_bytes()).map_err(nul_error)?;
            let Some(stat) = statat(dir, &name)? else {
                continue;
            };
            if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                let child = openat(
                    dir,
                    &name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )?;
                remove_dir_contents(child.as_raw_fd())?;
                // SAFETY: child was opened no-follow from this directory and
                // recursive removal has emptied it.
                if unsafe { libc::unlinkat(dir, name.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
                    return Err(io_context("remove contained child directory")(
                        std::io::Error::last_os_error(),
                    ));
                }
            } else {
                // SAFETY: unlinkat with flags 0 removes the observed final
                // non-directory entry without following a symlink.
                if unsafe { libc::unlinkat(dir, name.as_ptr(), 0) } < 0 {
                    return Err(io_context("remove contained child entry")(
                        std::io::Error::last_os_error(),
                    ));
                }
            }
        }
        sync_dir_fd(dir)
    })();
    // SAFETY: stream was returned by fdopendir and is closed exactly once.
    unsafe { libc::closedir(stream) };
    result
}

fn sync_dir_fd(dir: i32) -> Result<()> {
    // SAFETY: dup creates a fresh descriptor so File can own and close it.
    let duplicate = unsafe { libc::dup(dir) };
    if duplicate < 0 {
        return Err(io_context("duplicate contained directory")(
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: duplicate is non-negative and freshly returned by dup.
    let directory = unsafe { File::from_raw_fd(duplicate) };
    directory
        .sync_all()
        .map_err(io_context("fsync contained directory"))
}

fn write_at(dir: i32, name: &CString, bytes: &[u8], mode: u32) -> Result<()> {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp =
        CString::new(format!(".wt-tmp-{}-{sequence}", std::process::id())).map_err(nul_error)?;
    let fd = openat(
        dir,
        &tmp,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        mode,
    )?;
    let mut file = File::from(fd);
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(io_context("set contained file mode"))?;
    file.write_all(bytes)
        .map_err(io_context("write contained temporary"))?;
    file.sync_all()
        .map_err(io_context("fsync contained temporary"))?;
    // SAFETY: both names are single NUL-terminated components relative to the
    // same owned destination directory fd.
    let result = unsafe { libc::renameat(dir, tmp.as_ptr(), dir, name.as_ptr()) };
    if result < 0 {
        return Err(io_context("rename contained temporary")(
            std::io::Error::last_os_error(),
        ));
    }
    sync_dir_fd(dir)
}

fn copy_entry(
    source: &Path,
    destination_root: &Path,
    relative: &Path,
    report: &mut CopyReport,
) -> Result<()> {
    let metadata =
        std::fs::symlink_metadata(source).map_err(fs_error("inspect copy source", source))?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(source).map_err(fs_error("read copy symlink", source))?;
        let (parent, name) = open_parent_path(destination_root, relative, true)?;
        let target = CString::new(target.as_os_str().as_bytes()).map_err(nul_error)?;
        // SAFETY: both strings are NUL-terminated and `parent` is an owned
        // destination directory fd. symlinkat creates only the final entry.
        if unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) } < 0 {
            return Err(io_context("recreate contained copy symlink")(
                std::io::Error::last_os_error(),
            ));
        }
        sync_dir_fd(parent.as_raw_fd())?;
        report.symlinks.push(relative.to_path_buf());
    } else if metadata.is_dir() {
        create_directory_at(
            destination_root,
            relative,
            metadata.permissions().mode() & 0o777,
        )?;
        report.directories.push(relative.to_path_buf());
        let mut entries = std::fs::read_dir(source)
            .map_err(fs_error("read copy source directory", source))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(fs_error("read copy source entry", source))?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            copy_entry(
                &entry.path(),
                destination_root,
                &relative.join(name),
                report,
            )?;
        }
    } else if metadata.is_file() {
        let (parent, name) = open_parent_path(destination_root, relative, true)?;
        let mode = metadata.permissions().mode() & 0o777;
        copy_file_at(source, parent.as_raw_fd(), &name, mode)?;
        sync_dir_fd(parent.as_raw_fd())?;
        report.files.push(relative.to_path_buf());
    }
    Ok(())
}

fn create_directory_at(destination_root: &Path, relative: &Path, mode: u32) -> Result<()> {
    let (parent, name) = open_parent_path(destination_root, relative, true)?;
    // SAFETY: `parent` is an owned destination directory and `name` is one
    // NUL-terminated path component.
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) };
    let created = if result == 0 {
        true
    } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
        false
    } else {
        return Err(io_context("create contained copied directory")(
            std::io::Error::last_os_error(),
        ));
    };
    let directory = openat(
        parent.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    // SAFETY: `directory` is a valid owned fd; fchmod changes only its mode.
    if created && unsafe { libc::fchmod(directory.as_raw_fd(), mode as libc::mode_t) } < 0 {
        return Err(io_context("set copied directory mode")(
            std::io::Error::last_os_error(),
        ));
    }
    sync_dir_fd(parent.as_raw_fd())
}

fn copy_file_at(source: &Path, destination_dir: i32, name: &CString, mode: u32) -> Result<()> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)
        .map_err(fs_error("open copy source", source))?;
    let output = openat(
        destination_dir,
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        mode,
    )?;
    let mut output = File::from(output);
    output
        .set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(io_context("set copied file mode"))?;
    std::io::copy(&mut input, &mut output).map_err(io_context("copy contained file bytes"))?;
    output
        .sync_all()
        .map_err(io_context("fsync contained copied file"))
}

fn reject_symlink_components(root: &Path, relative: &Path) -> Result<()> {
    let root_meta =
        std::fs::symlink_metadata(root).map_err(fs_error("inspect containment root", root))?;
    if root_meta.file_type().is_symlink() {
        return Err(nofollow_error(
            root,
            std::io::Error::from_raw_os_error(libc::ELOOP),
        ));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(nofollow_error(
                    &current,
                    std::io::Error::from_raw_os_error(libc::ELOOP),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(fs_error("inspect contained path", &current)(error)),
        }
    }
    Ok(())
}

fn nofollow_error(path: &Path, error: std::io::Error) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "SYMLINK_REFUSED",
        format!("refusing no-follow access to {}: {error}", path.display()),
        "replace symlinked path components with owned directories or files",
    )
}

fn fs_error<'a>(
    action: &'static str,
    path: &'a Path,
) -> impl FnOnce(std::io::Error) -> CoreError + 'a {
    move |error| {
        CoreError::new(
            ExitClass::Internal,
            "IO_FAILED",
            format!("{action} {}: {error}", path.display()),
            "retry the operation and inspect filesystem permissions if it repeats",
        )
    }
}

fn io_context(action: &'static str) -> impl FnOnce(std::io::Error) -> CoreError {
    move |error| {
        CoreError::new(
            ExitClass::Internal,
            "IO_FAILED",
            format!("{action}: {error}"),
            "retry the operation and inspect filesystem permissions if it repeats",
        )
    }
}

fn nul_error(error: std::ffi::NulError) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "CONFIG_INVALID",
        format!("path contains NUL: {error}"),
        "use a path without NUL bytes",
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
    struct Doc {
        schema: u8,
        value: String,
    }

    #[test]
    fn executable_probe_judges_what_exec_would_run() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("tool");
        std::fs::write(&real, []).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.path().join("data");
        std::fs::write(&plain, []).unwrap();
        symlink(&real, dir.path().join("link")).unwrap();
        symlink(&plain, dir.path().join("link-to-data")).unwrap();
        symlink(dir.path().join("absent"), dir.path().join("dangling")).unwrap();
        symlink(dir.path(), dir.path().join("link-to-dir")).unwrap();

        assert!(is_executable_file(&real).unwrap());
        assert!(is_executable_file(&dir.path().join("link")).unwrap());
        assert!(!is_executable_file(&plain).unwrap());
        assert!(!is_executable_file(&dir.path().join("link-to-data")).unwrap());
        assert!(!is_executable_file(&dir.path().join("dangling")).unwrap());
        assert!(!is_executable_file(&dir.path().join("link-to-dir")).unwrap());
        assert!(!is_executable_file(&dir.path().join("absent")).unwrap());
    }

    #[test]
    fn store_is_private_and_never_exposes_a_partial_document() {
        let dir = tempdir().unwrap();
        let old = Doc {
            schema: 1,
            value: "old".into(),
        };
        let new = Doc {
            schema: 1,
            value: "new".into(),
        };
        for (index, expect_new) in [
            (0, false),
            (1, false),
            (2, false),
            (3, true),
            (4, true),
            (5, true),
        ] {
            let path = dir.path().join(format!("state/{index}.json"));
            STORE_FAIL_AFTER.store(u64::MAX, Ordering::Relaxed);
            write_json(&path, &old).unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            STORE_FAIL_AFTER.store(index, Ordering::Relaxed);
            assert!(write_json(&path, &new).is_err());
            let actual = read_json::<Doc>(&path, "STATE_CORRUPT").unwrap().unwrap();
            assert_eq!(&actual, if expect_new { &new } else { &old });
        }
        STORE_FAIL_AFTER.store(u64::MAX, Ordering::Relaxed);
    }

    #[test]
    fn nofollow_rejects_a_symlinked_parent_component() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        let rel = RelPath::new("link/secret").unwrap();
        let error = write_nofollow(dir.path(), &rel, b"no", 0o600).unwrap_err();
        assert_eq!(error.code.0, "SYMLINK_REFUSED");
        assert!(!outside.path().join("secret").exists());
    }

    #[test]
    fn recursive_copy_recreates_symlinks_and_copies_each_file() {
        let source = tempdir().unwrap();
        let destination = tempdir().unwrap();
        std::fs::create_dir(source.path().join("local")).unwrap();
        std::fs::write(source.path().join("local/file"), b"payload").unwrap();
        symlink("file", source.path().join("local/link")).unwrap();
        let report = copy_contained(
            source.path(),
            destination.path(),
            &RelPath::new("local").unwrap(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(destination.path().join("local/file")).unwrap(),
            b"payload"
        );
        assert_eq!(
            std::fs::read_link(destination.path().join("local/link")).unwrap(),
            Path::new("file")
        );
        assert_eq!(report.files, [PathBuf::from("local/file")]);
        assert_eq!(report.symlinks, [PathBuf::from("local/link")]);
    }

    #[test]
    fn recursive_copy_refuses_a_symlinked_destination_directory() {
        let source = tempdir().unwrap();
        let destination = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("cfg/sub")).unwrap();
        std::fs::write(source.path().join("cfg/sub/secret.json"), b"secret").unwrap();
        std::fs::create_dir(destination.path().join("cfg")).unwrap();
        symlink(outside.path(), destination.path().join("cfg/sub")).unwrap();

        let error = copy_contained(
            source.path(),
            destination.path(),
            &RelPath::new("cfg").unwrap(),
        )
        .unwrap_err();

        assert_eq!(error.code.0, "SYMLINK_REFUSED");
        assert!(!outside.path().join("secret.json").exists());
    }

    #[test]
    fn exclude_splice_and_log_retention_preserve_their_formats() {
        let dir = tempdir().unwrap();
        let exclude = dir.path().join("exclude");
        std::fs::write(&exclude, "human\n# >>> wt managed >>>\nstale\n").unwrap();
        let result = splice_exclude(
            &exclude,
            "# >>> wt managed >>>\n/.wt/\n# <<< wt managed <<<\n",
        )
        .unwrap();
        assert!(result.repaired);
        assert!(std::fs::read_to_string(exclude)
            .unwrap()
            .starts_with("human\n# >>>"));

        let logs = dir.path().join("logs");
        std::fs::create_dir(&logs).unwrap();
        for i in 0..4 {
            std::fs::write(logs.join(format!(".-build-2026010{i}.log")), []).unwrap();
        }
        let removed = retain_logs(&logs, ".", "build", 2).unwrap();
        assert_eq!(removed.len(), 3);
        assert_eq!(std::fs::read_dir(&logs).unwrap().count(), 1);

        for task in ["build", "build-fast", "build-20260103"] {
            for day in 1..=3 {
                std::fs::write(logs.join(format!(".-{task}-2026010{day}.log")), []).unwrap();
            }
        }
        let removed = retain_logs(&logs, ".", "build", 2).unwrap();
        assert_eq!(removed.len(), 2);
        assert_eq!(
            std::fs::read_dir(&logs)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains("build-fast"))
                .count(),
            3
        );
        assert_eq!(
            std::fs::read_dir(&logs)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".-build-20260103-")
                })
                .count(),
            3
        );
    }

    #[test]
    fn nofollow_observation_and_removal_preserve_symlink_boundaries() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir(root.path().join("owned")).unwrap();
        std::fs::write(root.path().join("owned/file"), b"bytes").unwrap();
        symlink(outside.path(), root.path().join("owned/link")).unwrap();

        assert_eq!(
            observe_nofollow(root.path(), &RelPath::new("owned/file").unwrap(), false).unwrap(),
            Observed::Regular(b"bytes".to_vec())
        );
        assert_eq!(
            observe_nofollow(root.path(), &RelPath::new("owned/link").unwrap(), false).unwrap(),
            Observed::Symlink
        );
        assert!(remove_nofollow(root.path(), &RelPath::new("owned/link").unwrap()).unwrap());
        assert!(outside.path().exists());
        assert!(remove_dir_all_nofollow(root.path(), &RelPath::new("owned").unwrap()).unwrap());
        assert!(!root.path().join("owned").exists());
    }

    #[test]
    fn adapter_snapshot_captures_only_requested_contents() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.path().join("secret"), "do-not-read").unwrap();
        let snapshot = capture_dir_snapshot(
            dir.path(),
            &RelPath::new(".").unwrap(),
            &["Cargo.toml".into()],
        )
        .unwrap();
        assert!(snapshot.names.contains("secret"));
        assert_eq!(snapshot.contents.len(), 1);
    }

    #[test]
    fn timestamps_use_one_rfc3339_utc_shape() {
        let value = timestamp().unwrap();
        assert_eq!(value.len(), 30);
        assert_eq!(&value[4..5], "-");
        assert_eq!(&value[7..8], "-");
        assert_eq!(&value[10..11], "T");
        assert_eq!(&value[19..20], ".");
        assert!(value.ends_with('Z'));
    }
}
