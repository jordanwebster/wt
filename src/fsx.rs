//! Small filesystem helpers.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::{Result, io_at};

/// Writes `contents` to `path` (creating parent directories) unless it
/// already holds exactly that, with that mode. Returns whether it wrote.
/// Writing goes through a temporary file and a rename, so a reader never
/// sees a half-written file.
pub fn write_if_changed(path: &Path, contents: &[u8], mode: Option<u32>) -> Result<bool> {
    if let Ok(existing) = fs::read(path) {
        let same_mode = match mode {
            None => true,
            Some(mode) => fs::metadata(path)
                .map(|m| m.permissions().mode() & 0o7777 == mode)
                .unwrap_or(false),
        };
        if existing == contents && same_mode {
            return Ok(false);
        }
    }
    write_atomic(path, contents, mode)?;
    Ok(true)
}

pub fn write_atomic(path: &Path, contents: &[u8], mode: Option<u32>) -> Result<()> {
    let temporary = write_temporary(path, contents, mode)?;
    let result = fs::rename(&temporary, path).map_err(io_at("replacing", path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Writes `contents` to `path` only if nothing is there, never replacing a
/// file that appears meanwhile, and returns whether it wrote. A finished
/// temporary file is hard-linked into place, which fails rather than
/// replaces, so a reader never sees it half-written either.
pub fn write_new(path: &Path, contents: &[u8], mode: Option<u32>) -> Result<bool> {
    let temporary = write_temporary(path, contents, mode)?;
    let linked = fs::hard_link(&temporary, path);
    let _ = fs::remove_file(&temporary);
    match linked {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        // A filesystem without hard links: create the file exclusively,
        // which still never replaces one.
        Err(_) => {
            let mut file = match File::options().write(true).create_new(true).open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Ok(false);
                }
                Err(error) => return Err(io_at("creating", path)(error)),
            };
            file.write_all(contents).map_err(io_at("writing", path))?;
            if let Some(mode) = mode {
                fs::set_permissions(path, fs::Permissions::from_mode(mode))
                    .map_err(io_at("setting the mode of", path))?;
            }
            Ok(true)
        }
    }
}

/// A finished file beside `path`, for [`write_atomic`] and [`write_new`] to
/// move into place.
fn write_temporary(path: &Path, contents: &[u8], mode: Option<u32>) -> Result<std::path::PathBuf> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(io_at("creating", parent))?;
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    // Unique per write, not just per process: an embedding program may
    // render the same tree from several threads.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let write = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = parent.join(format!(".{file_name}.wt-{}-{write}", std::process::id()));
    let result = (|| {
        let mut file = File::create(&temporary).map_err(io_at("writing", &temporary))?;
        file.write_all(contents)
            .map_err(io_at("writing", &temporary))?;
        if let Some(mode) = mode {
            fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
                .map_err(io_at("setting the mode of", &temporary))?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(temporary),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// Copies a file or directory, copy-on-write where the filesystem can clone.
pub fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(io_at("creating", parent))?;
    }
    if clone(from, to)? {
        return Ok(());
    }
    copy_recursive(from, to)
}

#[cfg(target_os = "macos")]
fn clone(from: &Path, to: &Path) -> Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    const CLONE_NOFOLLOW: u32 = 0x0001;
    let source = CString::new(from.as_os_str().as_bytes()).expect("paths have no NUL");
    let destination = CString::new(to.as_os_str().as_bytes()).expect("paths have no NUL");
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let result = unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), CLONE_NOFOLLOW) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOTSUP) | Some(libc::EXDEV) => Ok(false),
        _ => Err(io_at("cloning", from)(error)),
    }
}

#[cfg(not(target_os = "macos"))]
fn clone(_from: &Path, _to: &Path) -> Result<bool> {
    Ok(false)
}

fn copy_recursive(from: &Path, to: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(from).map_err(io_at("reading", from))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(from).map_err(io_at("reading", from))?;
        std::os::unix::fs::symlink(target, to).map_err(io_at("creating", to))
    } else if metadata.is_dir() {
        fs::create_dir_all(to).map_err(io_at("creating", to))?;
        for entry in fs::read_dir(from).map_err(io_at("reading", from))? {
            let entry = entry.map_err(io_at("reading", from))?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        fs::set_permissions(to, metadata.permissions()).map_err(io_at("copying the mode of", to))
    } else {
        fs::copy(from, to).map_err(io_at("copying", from))?;
        Ok(())
    }
}

/// Single-quotes a string for `sh`.
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_when_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b.txt");
        assert!(write_if_changed(&path, b"x", None).unwrap());
        assert!(!write_if_changed(&path, b"x", None).unwrap());
        assert!(write_if_changed(&path, b"x", Some(0o755)).unwrap());
        assert!(!write_if_changed(&path, b"x", Some(0o755)).unwrap());
        assert!(write_if_changed(&path, b"y", Some(0o755)).unwrap());
    }

    #[test]
    fn writes_new_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/new.txt");
        assert!(write_new(&path, b"first", Some(0o600)).unwrap());
        assert!(!write_new(&path, b"second", None).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(dir.path().join("a")).unwrap().count(), 1);
    }

    #[test]
    fn copies_directories() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        fs::create_dir_all(source.join("inner")).unwrap();
        fs::write(source.join("inner/file"), "hi").unwrap();
        copy(&source, &dir.path().join("out/dst")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("out/dst/inner/file")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn quotes() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }
}
