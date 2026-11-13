#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;
use wt::Wt;

/// A scratch directory holding a wt home and a git repository `app` with one
/// commit on `main`.
pub struct Fixture {
    _dir: TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub repo: PathBuf,
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let home = root.join("home");
        let repo = root.join("app");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("README"), "app\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Fixture {
            _dir: dir,
            root,
            home,
            repo,
        }
    }

    pub fn wt(&self) -> Wt {
        Wt::with_home(&self.home).unwrap()
    }

    /// Writes `.wt.toml` in the main checkout and commits it, so worktrees
    /// made afterwards have it too.
    pub fn config(&self, text: &str) {
        fs::write(self.repo.join(".wt.toml"), text).unwrap();
        git(&self.repo, &["add", ".wt.toml"]);
        git(&self.repo, &["commit", "-q", "-m", "config"]);
    }

    /// A plain `git worktree add` of a new branch `name`.
    pub fn worktree(&self, name: &str) -> PathBuf {
        let path = self.root.join("trees").join(name);
        git(
            &self.repo,
            &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
        );
        path
    }

    /// Runs the wt binary with this fixture's home.
    pub fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_wt"))
            .args(args)
            .current_dir(cwd)
            .env("WT_HOME", &self.home)
            .output()
            .unwrap()
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

pub fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}
