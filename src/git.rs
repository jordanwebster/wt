//! Git, called as a subprocess.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result, io_at};

/// Runs `git -C dir args…` and returns its trimmed stdout.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(io_at("running git in", dir))?;
    if !output.status.success() {
        return Err(Error::Git {
            args: args.join(" "),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The root of the worktree containing `path`.
pub fn toplevel(path: &Path) -> Result<PathBuf> {
    let root = run(path, &["rev-parse", "--show-toplevel"])
        .map_err(|_| Error::NotFound(format!("{} is not inside a git worktree", path.display())))?;
    canonical(Path::new(&root))
}

/// The repository's main checkout: the worktree that owns the shared git
/// directory.
pub fn main_checkout(path: &Path) -> Result<PathBuf> {
    let common = PathBuf::from(run(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    match common.file_name() {
        Some(name) if name == ".git" => canonical(common.parent().expect(".git has a parent")),
        _ => Err(Error::Conflict(format!(
            "{} belongs to a bare repository, which wt doesn't support",
            path.display()
        ))),
    }
}

/// The shared git directory, where `info/exclude` lives.
pub fn common_dir(path: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(run(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?))
}

/// The checked-out branch, or `None` when HEAD is detached.
pub fn current_branch(path: &Path) -> Option<String> {
    run(path, &["branch", "--show-current"])
        .ok()
        .filter(|branch| !branch.is_empty())
}

pub fn canonical(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(io_at("resolving", path))
}
