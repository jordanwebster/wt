//! Git, called as a subprocess.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result, io_at};

/// The git executable. `/usr/bin/git` on macOS is a stub that locates the
/// developer tools on every call, which costs ~35 ms each time; asking it
/// once where git really lives makes the rest of a run's calls direct.
fn binary() -> &'static std::path::Path {
    static BINARY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| {
        let real = Command::new("git")
            .arg("--exec-path")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("git")
            });
        match real {
            Some(path) if path.is_file() => path,
            _ => PathBuf::from("git"),
        }
    })
}

/// Runs `git -C dir args…` and returns its trimmed stdout.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(binary())
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

/// Like [`run`], but kills git and fails if it takes longer than `timeout`.
pub fn run_timeout(dir: &Path, args: &[&str], timeout: std::time::Duration) -> Result<String> {
    use std::process::Stdio;
    let child = Command::new(binary())
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_at("running git in", dir))?;
    let pid = child.id();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    let output = match receiver.recv_timeout(timeout) {
        Ok(output) => output.map_err(io_at("running git in", dir))?,
        Err(_) => {
            // SAFETY: kill only sends a signal; the pid is our own child,
            // which the waiting thread has not reaped yet.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            return Err(Error::Git {
                args: args.join(" "),
                message: format!("timed out after {}s", timeout.as_secs()),
            });
        }
    };
    if !output.status.success() {
        return Err(Error::Git {
            args: args.join(" "),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn ref_exists(dir: &Path, name: &str) -> bool {
    run(dir, &["show-ref", "--verify", "--quiet", name]).is_ok()
}

pub fn has_origin(dir: &Path) -> bool {
    run(dir, &["remote", "get-url", "origin"]).is_ok()
}

/// The branch `origin/HEAD` points at, falling back to `main` or `master`
/// when the remote doesn't say.
pub fn default_branch(dir: &Path) -> Option<String> {
    if let Ok(head) = run(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) && let Some(branch) = head.strip_prefix("origin/")
    {
        return Some(branch.to_owned());
    }
    ["main", "master"]
        .into_iter()
        .find(|branch| ref_exists(dir, &format!("refs/remotes/origin/{branch}")))
        .map(str::to_owned)
}

/// Whether the worktree has changes or untracked files git doesn't ignore.
pub fn is_dirty(dir: &Path) -> Result<bool> {
    // Explicit flags, because status.showUntrackedFiles=no (common in large
    // repositories) would otherwise hide files that removal then deletes.
    let args = [
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--ignore-submodules=none",
    ];
    Ok(!run(dir, &args)?.is_empty())
}
