//! Bounded discovery of git checkouts already on the machine (A76, §14.7).
//!
//! The walk is bounded by depth and by refusing to descend past a checkout,
//! not by a curated list of directories to look in: a list finds nothing in
//! the home of anyone who named their directories differently, while the depth
//! bound costs one readdir per directory that sits *above* a checkout.

use std::collections::VecDeque;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wt_core::setup::CandidateKind;

use crate::Result;

/// How deep below a root a checkout is still found.
pub const DEFAULT_DEPTH: usize = 6;

/// Directories that never contain a checkout worth offering, and do contain
/// enormous numbers of directories that do not.
const SKIP: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "build",
    "dist",
    "Library",
    "Applications",
    "Pods",
    ".cache",
    ".venv",
    ".Trash",
];

/// One directory the walk recognised as git's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    pub kind: CandidateKind,
    /// The common gitdir: the `.git` directory shared by a checkout and every
    /// worktree linked to it.
    pub common_gitdir: PathBuf,
    /// Seconds since the epoch of the most recent git activity here.
    pub touched: u64,
}

/// How far the walk has got, for the progress line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Progress {
    pub directories: usize,
    pub found: usize,
    pub finished: bool,
    /// The budget expired before the walk reached everything in range, so the
    /// result is a sample rather than a survey.
    pub truncated: bool,
}

/// Walks `roots`, reporting every checkout and linked worktree below them.
///
/// `report` is called as the walk proceeds so a caller can show progress; it
/// is never called from another thread.
pub fn discover(
    roots: &[PathBuf],
    depth: usize,
    budget: Duration,
    report: &mut dyn FnMut(Progress),
) -> Result<Vec<Found>> {
    let started = Instant::now();
    // Each entry carries the device of the root it descends from: a pooled
    // list would let a mount from root B be entered underneath root A.
    let mut queue: VecDeque<(PathBuf, usize, Option<u64>)> = VecDeque::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for root in roots {
        let Ok(root) = crate::fsx::canonicalize(root) else {
            continue;
        };
        if seen.iter().any(|other| root.starts_with(other)) {
            continue;
        }
        let device = std::fs::symlink_metadata(&root).ok().map(|meta| meta.dev());
        seen.push(root.clone());
        queue.push_back((root, 0, device));
    }

    let mut found = Vec::new();
    let mut progress = Progress::default();
    let mut since_report = 0usize;
    let mut truncated = false;
    while let Some((directory, level, device)) = queue.pop_front() {
        if started.elapsed() > budget {
            // The caller says so rather than presenting a partial sweep as a
            // complete one.
            truncated = true;
            break;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            // An unreadable directory is ordinary on a real machine; the walk
            // is a survey, not an audit.
            continue;
        };
        progress.directories += 1;
        let mut children = Vec::new();
        let mut git = None;
        let mut gitdir_markers = 0;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if name == ".git" {
                git = Some((entry.path(), kind.is_dir()));
                continue;
            }
            // A bare or mirror clone is a gitdir with no working tree: it has
            // nothing to register, and descending it finds only object shards.
            // `config` is required alongside the rest because "HEAD, objects,
            // refs" alone is a plausible set of names in an ordinary project.
            if matches!(name, "HEAD" | "objects" | "refs" | "config") {
                gitdir_markers += 1;
            }
            // `file_type` comes from the directory entry, so a symlink reads
            // as a symlink and is never followed.
            if !kind.is_dir() || name.starts_with('.') || SKIP.contains(&name) {
                continue;
            }
            children.push(entry.path());
        }

        if git.is_none() && gitdir_markers == 4 {
            continue;
        }

        if let Some((git_path, is_dir)) = git {
            // A checkout is a leaf: whatever is below it belongs to it.
            if let Some(entry) = classify(&directory, &git_path, is_dir) {
                found.push(entry);
                progress.found += 1;
            }
            since_report += 1;
            if since_report >= 64 {
                since_report = 0;
                report(progress);
            }
            continue;
        }

        if level >= depth {
            continue;
        }
        for child in children {
            if let Some(device) = device {
                let Ok(meta) = std::fs::symlink_metadata(&child) else {
                    continue;
                };
                if meta.dev() != device {
                    continue;
                }
            }
            queue.push_back((child, level + 1, device));
        }
        since_report += 1;
        if since_report >= 64 {
            since_report = 0;
            report(progress);
        }
    }
    progress.finished = true;
    progress.truncated = truncated;
    report(progress);
    found.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(found)
}

/// Decides what a `.git` entry means, and where the shared gitdir is.
fn classify(directory: &Path, git_path: &Path, is_dir: bool) -> Option<Found> {
    if is_dir {
        // Bare clones never reach here: they carry no `.git` entry and the
        // walk recognises them by their gitdir markers instead.
        return Some(Found {
            path: directory.to_owned(),
            kind: CandidateKind::Checkout,
            common_gitdir: git_path.to_owned(),
            touched: touched(git_path),
        });
    }

    let contents = std::fs::read_to_string(git_path).ok()?;
    let pointer = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    let gitdir = if Path::new(pointer).is_absolute() {
        PathBuf::from(pointer)
    } else {
        directory.join(pointer)
    };
    // A submodule's gitdir is `<super>/.git/modules/<name>`: it is a
    // repository of its own, not a worktree of anything wt would register.
    // The two components must be adjacent — a checkout at `~/modules/api`
    // would otherwise lose every worktree it has.
    if has_adjacent(&gitdir, ".git", "modules") {
        return None;
    }
    let common = common_gitdir(&gitdir)?;
    Some(Found {
        path: directory.to_owned(),
        kind: CandidateKind::Linked,
        common_gitdir: common,
        touched: touched(&gitdir),
    })
}

/// Whether `first` is immediately followed by `second` in a path.
fn has_adjacent(path: &Path, first: &str, second: &str) -> bool {
    let parts: Vec<_> = path.components().map(|part| part.as_os_str()).collect();
    parts
        .windows(2)
        .any(|pair| pair[0] == first && pair[1] == second)
}

/// Resolves a linked worktree's gitdir to the gitdir it shares.
///
/// git records this in `commondir`; the layout below `worktrees` is the
/// fallback for a repository written by something that omitted it.
fn common_gitdir(gitdir: &Path) -> Option<PathBuf> {
    if let Ok(relative) = std::fs::read_to_string(gitdir.join("commondir")) {
        let relative = relative.trim();
        if !relative.is_empty() {
            let resolved = if Path::new(relative).is_absolute() {
                PathBuf::from(relative)
            } else {
                gitdir.join(relative)
            };
            return crate::fsx::canonicalize(&resolved).ok().or(Some(resolved));
        }
    }
    let mut parts = gitdir.components().collect::<Vec<_>>();
    let index = parts
        .iter()
        .rposition(|part| part.as_os_str() == "worktrees")?;
    parts.truncate(index);
    Some(parts.iter().collect())
}

/// The most recent git activity, from the two files git touches on nearly
/// every operation — including a bare `git status`, which writes the index.
fn touched(gitdir: &Path) -> u64 {
    ["index", "logs/HEAD"]
        .iter()
        .filter_map(|name| std::fs::metadata(gitdir.join(name)).ok())
        .filter_map(|meta| u64::try_from(meta.mtime()).ok())
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn repo(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(path.join(".git")).unwrap();
        std::fs::write(path.join(".git/index"), b"").unwrap();
        path
    }

    fn walk(roots: &[PathBuf]) -> Vec<Found> {
        discover(roots, DEFAULT_DEPTH, Duration::from_secs(30), &mut |_| {}).unwrap()
    }

    /// The walk canonicalises its roots, so expectations must too — on macOS
    /// a temporary directory is reached through a symlinked `/var`.
    fn real(path: &Path) -> PathBuf {
        crate::fsx::canonicalize(path).unwrap()
    }

    #[test]
    fn a_checkout_is_found_and_not_descended_into() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outer = repo(root, "outer");
        // A nested checkout below one already found is not offered separately:
        // whatever is inside a checkout belongs to it.
        std::fs::create_dir_all(outer.join("inner/.git")).unwrap();
        let found = walk(&[root.to_owned()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, real(&outer));
        assert_eq!(found[0].kind, CandidateKind::Checkout);
    }

    #[test]
    fn a_linked_worktree_resolves_to_its_shared_gitdir() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let main = repo(root, "main");
        let gitdir = main.join(".git/worktrees/feature");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(gitdir.join("index"), b"").unwrap();
        let linked = root.join("feature");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        let found = walk(&[root.to_owned()]);
        let linked_found = found
            .iter()
            .find(|entry| entry.kind == CandidateKind::Linked)
            .expect("the linked worktree is found");
        let main_found = found
            .iter()
            .find(|entry| entry.kind == CandidateKind::Checkout)
            .expect("the checkout is found");
        assert_eq!(
            crate::fsx::canonicalize(&linked_found.common_gitdir).unwrap(),
            crate::fsx::canonicalize(&main_found.common_gitdir).unwrap(),
            "a worktree and its checkout must group together"
        );
    }

    #[test]
    fn a_submodule_is_not_offered() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let super_project = repo(root, "super");
        let gitdir = super_project.join(".git/modules/lib");
        std::fs::create_dir_all(&gitdir).unwrap();
        let sub = super_project.join("lib");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        let found = walk(&[root.to_owned()]);
        assert_eq!(found.len(), 1, "only the superproject is offered");
        assert_eq!(found[0].path, real(&super_project));
    }

    #[test]
    fn a_bare_checkout_is_neither_offered_nor_descended() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        // A bare clone is a gitdir at the top level: no `.git`, no worktree.
        let bare = root.join("mirror.git");
        std::fs::create_dir_all(bare.join("objects")).unwrap();
        std::fs::create_dir_all(bare.join("refs")).unwrap();
        std::fs::write(bare.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::write(bare.join("config"), b"[core]\n\tbare = true\n").unwrap();
        // Anything that looks like a checkout below it must stay unreported.
        repo(&bare, "buried");
        assert!(walk(&[root.to_owned()]).is_empty());
    }

    #[test]
    fn an_ordinary_project_is_not_mistaken_for_a_bare_clone() {
        // "HEAD", "objects" and "refs" are plausible names in a project that
        // has nothing to do with git's own layout.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let project = root.join("docs");
        std::fs::create_dir_all(project.join("objects")).unwrap();
        std::fs::create_dir_all(project.join("refs")).unwrap();
        std::fs::write(project.join("HEAD"), b"chapter\n").unwrap();
        repo(&project, "inner");
        let found = walk(&[root.to_owned()]);
        assert_eq!(found.len(), 1, "the checkout below it is still found");
        assert_eq!(found[0].path, real(&project.join("inner")));
    }

    #[test]
    fn a_checkout_under_a_directory_named_modules_keeps_its_worktrees() {
        // Only git's own `.git/modules` layout means submodule; a repository
        // that merely lives under `~/modules` must not lose its worktrees.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("modules");
        let main = repo(&root, "api");
        let gitdir = main.join(".git/worktrees/feature");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let linked = root.join("api-feature");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        let found = walk(std::slice::from_ref(&root));
        assert!(
            found
                .iter()
                .any(|entry| entry.kind == CandidateKind::Linked),
            "the worktree is still discovered: {found:?}"
        );
    }

    #[test]
    fn skip_listed_and_hidden_directories_are_not_entered() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        repo(&root.join("node_modules"), "buried");
        repo(&root.join(".hidden"), "buried");
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        assert!(walk(&[root.to_owned()]).is_empty());
    }

    #[test]
    fn the_depth_bound_stops_the_walk() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        repo(&root.join("a/b/c/d/e"), "deep");
        let shallow =
            discover(&[root.to_owned()], 2, Duration::from_secs(30), &mut |_| {}).unwrap();
        assert!(shallow.is_empty(), "a bound that finds nothing is honoured");
        assert_eq!(walk(&[root.to_owned()]).len(), 1);
    }

    #[test]
    fn a_symlinked_directory_is_never_followed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let real = temp.path().join("elsewhere");
        repo(&real, "target");
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();
        let found = discover(
            &[root.join("visible")],
            DEFAULT_DEPTH,
            Duration::from_secs(30),
            &mut |_| {},
        )
        .unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn an_unreadable_directory_does_not_stop_the_walk() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        repo(root, "readable");
        let found = walk(&[root.to_owned()]);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(found.len(), 1, "the readable checkout is still reported");
    }

    #[test]
    fn recency_comes_from_the_index_and_the_reflog() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let path = repo(root, "one");
        std::fs::create_dir_all(path.join(".git/logs")).unwrap();
        std::fs::write(path.join(".git/logs/HEAD"), b"x").unwrap();
        let found = walk(&[root.to_owned()]);
        assert!(found[0].touched > 0);
    }

    #[test]
    fn progress_reports_completion_exactly_once() {
        let temp = tempfile::tempdir().unwrap();
        repo(temp.path(), "one");
        let mut finals = 0;
        discover(
            &[temp.path().to_owned()],
            DEFAULT_DEPTH,
            Duration::from_secs(30),
            &mut |progress| {
                if progress.finished {
                    finals += 1;
                }
            },
        )
        .unwrap();
        assert_eq!(finals, 1);
    }

    #[test]
    fn a_root_below_another_root_is_walked_once() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        repo(&root.join("nested"), "one");
        let found = walk(&[root.to_owned(), root.join("nested")]);
        assert_eq!(found.len(), 1);
    }
}
