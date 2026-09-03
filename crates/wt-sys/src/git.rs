use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wt_core::model::duration_millis;
use wt_core::settings::GitTimeouts;
use wt_core::{CoreError, ExitClass};

use crate::proc::{self, CommandRequest, ProcessOutput};
use crate::Result;

pub use wt_core::from_ref::{normalize_url, pr_refspec, AddSpec, RefCandidates, Resolution};
pub type FromRef = Resolution;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Class {
    Query,
    Fetch,
    Clone,
    Worktree,
    Submodule,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadlines {
    pub query: Duration,
    pub fetch: Duration,
    pub clone: Duration,
    pub worktree: Duration,
    pub submodule: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self::from_settings(&GitTimeouts::default())
            .expect("wt-core's built-in git deadlines are valid")
    }
}

impl Deadlines {
    /// Converts wt-core's resolved settings into effect-layer durations.
    pub fn from_settings(settings: &GitTimeouts) -> Result<Self> {
        Ok(Self {
            query: resolved_duration(settings.query.as_deref(), "git.timeouts.query")?,
            fetch: resolved_duration(settings.fetch.as_deref(), "git.timeouts.fetch")?,
            clone: resolved_duration(settings.clone.as_deref(), "git.timeouts.clone")?,
            worktree: resolved_duration(settings.worktree.as_deref(), "git.timeouts.worktree")?,
            submodule: resolved_duration(settings.submodule.as_deref(), "git.timeouts.submodule")?,
        })
    }

    pub fn for_class(self, class: Class) -> Duration {
        match class {
            Class::Query => self.query,
            Class::Fetch => self.fetch,
            Class::Clone => self.clone,
            Class::Worktree => self.worktree,
            Class::Submodule => self.submodule,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Git {
    repo: PathBuf,
    program: OsString,
    deadlines: Deadlines,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusEntry {
    pub index: char,
    pub worktree: char,
    pub path: PathBuf,
    pub original_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AheadBehind {
    pub ahead: u64,
    pub behind: u64,
}

/// One branch's upstream name and whether that upstream is gone, read
/// together so callers pay one subprocess instead of two.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpstreamInfo {
    pub name: Option<String>,
    pub gone: bool,
}

impl Git {
    /// Opens a git effects handle without spending a validation subprocess.
    pub fn open(repo: &Path, deadlines: Deadlines) -> Result<Self> {
        Self::open_with_program(repo, "git", deadlines)
    }

    /// Opens with an alternate git executable without querying the repository.
    pub fn open_with_program(
        repo: &Path,
        program: impl Into<OsString>,
        deadlines: Deadlines,
    ) -> Result<Self> {
        Ok(Self {
            repo: repo.to_path_buf(),
            program: program.into(),
            deadlines,
        })
    }

    /// Opens and validates a worktree for register/adopt-style entry points.
    pub fn open_validated(repo: &Path, deadlines: Deadlines) -> Result<Self> {
        Self::open_validated_with_program(repo, "git", deadlines)
    }

    /// Validating constructor with an alternate executable for tests.
    pub fn open_validated_with_program(
        repo: &Path,
        program: impl Into<OsString>,
        deadlines: Deadlines,
    ) -> Result<Self> {
        let git = Self::open_with_program(repo, program, deadlines)?;
        let output = git.invoke(Class::Query, &["rev-parse", "--is-inside-work-tree"])?;
        if text(&output.stdout) != "true" {
            return Err(CoreError::new(
                ExitClass::State,
                "NOT_A_WORKTREE",
                format!("{} is not a git worktree", repo.display()),
                "choose a path inside a git worktree",
            ));
        }
        Ok(git)
    }

    /// Reads the installed git semantic version.
    pub fn version(program: impl Into<OsString>, timeout: Duration) -> Result<(u32, u32, u32)> {
        let mut request = CommandRequest::new(program);
        request.args = proc::os_args(&["--version"]);
        request.env.insert("LC_ALL".into(), "C".into());
        let output = proc::capture(&request, timeout)?;
        ensure_success(Class::Query, &request.args, output).map(|output| {
            let version_text = text(&output.stdout);
            let version = version_text
                .split_whitespace()
                .find(|part| part.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
                .unwrap_or_default();
            let mut parts = version.split('.');
            let major = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
            let minor = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
            let patch = parts
                .next()
                .and_then(|part| part.split('-').next())
                .and_then(|part| part.parse().ok())
                .unwrap_or(0);
            (major, minor, patch)
        })
    }

    /// Returns the canonical common git directory.
    pub fn common_dir(&self) -> Result<PathBuf> {
        let value = self.text(Class::Query, &["rev-parse", "--git-common-dir"])?;
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            self.repo.join(path)
        };
        path.canonicalize()
            .map_err(|error| git_io("canonicalize common gitdir", error))
    }

    /// Returns the canonical worktree root.
    pub fn toplevel(&self) -> Result<PathBuf> {
        PathBuf::from(self.text(Class::Query, &["rev-parse", "--show-toplevel"])?)
            .canonicalize()
            .map_err(|error| git_io("canonicalize worktree root", error))
    }

    /// Runs `git worktree list --porcelain` and parses its stable format.
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let output = self.invoke(Class::Query, &["worktree", "list", "--porcelain"])?;
        parse_worktree_porcelain(&String::from_utf8_lossy(&output.stdout))
    }

    /// Executes one bounded `git worktree add` derived by wt-core.
    pub fn worktree_add(&self, path: &Path, spec: &AddSpec) -> Result<()> {
        let mut args = vec![OsString::from("worktree"), OsString::from("add")];
        match spec {
            AddSpec::ExistingBranch(branch) => {
                args.push(path.as_os_str().to_owned());
                args.push(branch.into());
            }
            AddSpec::NewBranch { name, start, track } => {
                args.extend([OsString::from("-b"), OsString::from(name)]);
                // Explicit either way: git tracks a remote start by default
                // but a local one only when told to, and the caller decided.
                args.push(OsString::from(if *track {
                    "--track"
                } else {
                    "--no-track"
                }));
                args.push(path.as_os_str().to_owned());
                args.push(start.into());
            }
            AddSpec::Detached { start } => {
                args.push(OsString::from("--detach"));
                args.push(path.as_os_str().to_owned());
                args.push(start.into());
            }
        }
        // Parallel checkout: one worker per core populates the new tree.
        // Passed through the environment (git >= 2.31, wt's floor) so the
        // timing log still names the operation `worktree add`.
        let extra_env = [
            ("GIT_CONFIG_COUNT".to_owned(), "1".to_owned()),
            ("GIT_CONFIG_KEY_0".to_owned(), "checkout.workers".to_owned()),
            ("GIT_CONFIG_VALUE_0".to_owned(), "0".to_owned()),
        ];
        let output =
            self.invoke_request(&self.repo, Class::Worktree, args.clone(), false, &extra_env)?;
        ensure_success(Class::Worktree, &args, output)?;
        crate::failpoint::new_g()
    }

    /// Best-effort enablement of git's own exact status caches for one
    /// freshly created worktree: the untracked cache everywhere, plus the
    /// built-in filesystem monitor where git supports it (macOS and
    /// Windows, git >= 2.37). Scoped through `extensions.worktreeConfig`
    /// so no other checkout of the repository changes behaviour. Both
    /// caches keep `git status` output exact — git invalidates them
    /// itself — they only shrink the working-tree scan.
    pub fn accelerate_status(&self, tree: &Path) -> Result<()> {
        let extension = self.invoke_status(
            Class::Query,
            &[
                "config",
                "--type=bool",
                "--get",
                "extensions.worktreeConfig",
            ],
        )?;
        if !(extension.child.code == Some(0) && text(&extension.stdout) == "true") {
            // Only when wt itself must enable the extension: a repository
            // carrying `core.bare = true` or `core.worktree` would need
            // those keys relocated first, so leave such a checkout alone.
            // With the extension already on, any such keys are already
            // where git prescribes and no migration question arises.
            let bare = self.invoke_status(
                Class::Query,
                &["config", "--type=bool", "--get", "core.bare"],
            )?;
            match bare.child.code {
                // Unset, or an explicit false: safe to proceed.
                Some(1) => {}
                Some(0) if text(&bare.stdout) == "false" => {}
                // True in any of git's spellings, an unparsable value, or a
                // failed query: leave the checkout alone.
                _ => return Ok(()),
            }
            let core_worktree =
                self.invoke_status(Class::Query, &["config", "--get", "core.worktree"])?;
            if core_worktree.child.code == Some(0) {
                return Ok(());
            }
            self.invoke(
                Class::Worktree,
                &["config", "extensions.worktreeConfig", "true"],
            )?;
        }
        let set = |key: &str, value: &str| -> Result<()> {
            let args = proc::os_args(&["config", "--worktree", key, value]);
            let output = self.invoke_request(tree, Class::Worktree, args.clone(), false, &[])?;
            ensure_success(Class::Worktree, &args, output).map(|_| ())
        };
        set("core.untrackedCache", "true")?;
        if cfg!(any(target_os = "macos", windows))
            && Self::version(self.program.clone(), self.deadlines.query)? >= (2, 37, 0)
        {
            set("core.fsmonitor", "true")?;
        }
        Ok(())
    }

    /// Reads one effective boolean config value as seen from the worktree
    /// at `tree`. Unset is `None`; a malformed value also reads as `None`
    /// because callers use this as a best-effort observation, never as a
    /// gate on real work.
    pub fn config_bool_in(&self, tree: &Path, key: &str) -> Result<Option<bool>> {
        let args = proc::os_args(&["config", "--type=bool", "--get", key]);
        let output = self.invoke_request(tree, Class::Query, args, false, &[])?;
        match output.child.code {
            Some(0) => Ok(Some(text(&output.stdout) == "true")),
            _ => Ok(None),
        }
    }

    /// Executes one bounded forced or ordinary `git worktree remove`.
    pub fn worktree_remove(&self, path: &Path, force: bool) -> Result<()> {
        let mut args = vec![OsString::from("worktree"), OsString::from("remove")];
        if force {
            args.push(OsString::from("--force"));
        }
        args.push(path.as_os_str().to_owned());
        self.invoke_os(Class::Worktree, args).map(|_| ())
    }

    /// Executes one bounded `git worktree prune`.
    pub fn worktree_prune(&self) -> Result<()> {
        self.invoke(Class::Worktree, &["worktree", "prune"])
            .map(|_| ())
    }

    /// Executes one bounded `git worktree repair` for the supplied paths.
    pub fn worktree_repair(&self, paths: &[PathBuf]) -> Result<()> {
        let mut args = vec![OsString::from("worktree"), OsString::from("repair")];
        args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
        self.invoke_os(Class::Worktree, args).map(|_| ())
    }

    /// Fetches one refspec with the fetch-class deadline and hygiene environment.
    pub fn fetch(&self, remote: &str, refspec: &str) -> Result<()> {
        self.invoke(Class::Fetch, &["fetch", remote, refspec])
            .map(|_| ())
    }

    /// Fetches ordinary origin branches without changing any local branch.
    pub fn fetch_origin_branches(&self) -> Result<()> {
        self.fetch("origin", "+refs/heads/*:refs/remotes/origin/*")
    }

    /// Fetches the named origin branches in one command. Fails when any
    /// branch does not exist on the remote; callers fall back to the
    /// wildcard fetch to preserve the resolve-anything behaviour.
    pub fn fetch_origin_named(&self, branches: &BTreeSet<String>) -> Result<()> {
        let mut args = vec![OsString::from("fetch"), OsString::from("origin")];
        args.extend(branches.iter().map(|branch| {
            OsString::from(format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"))
        }));
        self.invoke_os(Class::Fetch, args).map(|_| ())
    }

    /// Fetches a forge pull request, including the unknown-host fallback chain.
    pub fn fetch_pull_request(&self, host: &str, number: u64) -> Result<String> {
        let mut last_error = None;
        let mappings = pr_refspec(host, number);
        for (source, destination) in &mappings {
            match self.fetch("origin", &format!("+{source}:{destination}")) {
                Ok(()) => return Ok(destination.clone()),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.expect("every forge mapping has at least one refspec"))
    }

    /// Updates submodules under the longer submodule class deadline.
    pub fn submodule_update(&self) -> Result<()> {
        self.invoke(
            Class::Submodule,
            &["submodule", "update", "--init", "--recursive"],
        )
        .map(|_| ())
    }

    /// Hashes named files with one `git hash-object` query.
    ///
    /// Adapter input lists include alternative lockfiles and marker patterns.
    /// Missing entries still need a stable value so their later appearance is
    /// observable, but they must not make an otherwise valid sync fail.
    pub fn hash_object(&self, paths: &[PathBuf]) -> Result<Vec<String>> {
        let present = paths
            .iter()
            .enumerate()
            .filter(|(_, path)| self.repo.join(path).exists())
            .collect::<Vec<_>>();
        if present.is_empty() {
            return Ok(vec!["<missing>".to_owned(); paths.len()]);
        }
        let mut args = vec![OsString::from("hash-object"), OsString::from("--")];
        args.extend(present.iter().map(|(_, path)| path.as_os_str().to_owned()));
        let output = self.invoke_os(Class::Query, args)?;
        let hashes = lines(&output.stdout);
        if hashes.len() != present.len() {
            return Err(format_error(
                "git hash-object returned a different number of hashes than inputs",
            ));
        }
        let mut output = vec!["<missing>".to_owned(); paths.len()];
        for ((index, _), hash) in present.into_iter().zip(hashes) {
            output[index] = hash;
        }
        Ok(output)
    }

    /// Returns the requested paths that git reports as tracked with one query.
    pub fn tracked_paths(&self, tree: &Path, paths: &[PathBuf]) -> Result<BTreeSet<PathBuf>> {
        let mut args = vec![
            OsString::from("ls-files"),
            OsString::from("-z"),
            OsString::from("--"),
        ];
        args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
        let output = self.invoke_in(tree, Class::Query, args)?;
        Ok(output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| PathBuf::from(OsString::from_vec(part.to_vec())))
            .collect())
    }

    /// Returns porcelain-v1 status entries including untracked files.
    ///
    /// Status is the one query allowed to take git's optional locks: with
    /// them, git persists the refreshed stat and untracked-cache data it
    /// just computed, which is exactly what keeps the next status cheap.
    /// Each worktree has its own index, so concurrent statuses of
    /// different trees do not contend.
    pub fn status_porcelain(&self, tree: &Path) -> Result<Vec<StatusEntry>> {
        let args = proc::os_args(&["status", "--porcelain=v1", "-z", "--untracked-files=normal"]);
        let output = self.invoke_request(tree, Class::Query, args.clone(), true, &[])?;
        let output = ensure_success(Class::Query, &args, output)?;
        parse_status_porcelain(&output.stdout)
    }

    /// Whether any tracked file in `tree` is modified, staged or deleted;
    /// untracked files do not count. This is the bar a canonical must
    /// clear before wt moves it (§11.10).
    pub fn tracked_modified(&self, tree: &Path) -> Result<bool> {
        Ok(self
            .status_porcelain(tree)?
            .iter()
            .any(|entry| !(entry.index == '?' && entry.worktree == '?')))
    }

    /// Fast-forwards the checkout at `tree` to `reference`. `Ok(false)`
    /// when git refuses because it would not be a fast-forward or would
    /// overwrite local changes; the checkout is then untouched.
    pub fn merge_ff_only(&self, tree: &Path, reference: &str) -> Result<bool> {
        let args = proc::os_args(&["merge", "--ff-only", "--quiet", reference]);
        let output = self.invoke_status_in(tree, Class::Worktree, args.clone())?;
        match output.child.code {
            Some(0) => Ok(true),
            Some(1 | 128) if !output.timed_out => Ok(false),
            _ => Err(command_failed(Class::Worktree, &args, &output)),
        }
    }

    /// Moves a branch that no worktree holds to `commit`, guarded by the
    /// value it is expected to hold now.
    pub fn update_ref(&self, reference: &str, commit: &str, expected: &str) -> Result<()> {
        self.invoke(Class::Query, &["update-ref", reference, commit, expected])
            .map(|_| ())
    }

    /// Detaches the checkout at `tree` onto `revision`, updating its files.
    /// `Ok(false)` when git refuses because local changes would be lost.
    pub fn checkout_detach(&self, tree: &Path, revision: &str) -> Result<bool> {
        let args = proc::os_args(&["checkout", "--quiet", "--detach", revision, "--"]);
        let output = self.invoke_status_in(tree, Class::Worktree, args.clone())?;
        match output.child.code {
            Some(0) => Ok(true),
            Some(1) if !output.timed_out => Ok(false),
            _ => Err(command_failed(Class::Worktree, &args, &output)),
        }
    }

    /// The commit `tree`'s HEAD names.
    pub fn head_oid_in(&self, tree: &Path) -> Result<String> {
        let args = proc::os_args(&["rev-parse", "--verify", "HEAD"]);
        let output = self.invoke_in(tree, Class::Query, args)?;
        Ok(text(&output.stdout))
    }

    /// Counts commits in one rev-list range.
    pub fn rev_list_count(&self, range: &str) -> Result<u64> {
        self.text(Class::Query, &["rev-list", "--count", range])?
            .parse()
            .map_err(|error| parse_error("rev-list count", error))
    }

    /// Returns left/right counts for `left...right` as ahead/behind.
    pub fn ahead_behind(&self, left: &str, right: &str) -> Result<AheadBehind> {
        let range = format!("{left}...{right}");
        let value = self.text(
            Class::Query,
            &["rev-list", "--left-right", "--count", &range],
        )?;
        let mut counts = value.split_whitespace();
        Ok(AheadBehind {
            ahead: counts
                .next()
                .unwrap_or_default()
                .parse()
                .map_err(|error| parse_error("ahead count", error))?,
            behind: counts
                .next()
                .unwrap_or_default()
                .parse()
                .map_err(|error| parse_error("behind count", error))?,
        })
    }

    /// Lists drift paths from a three-dot diff, optionally path-limited.
    pub fn diff_name_only(
        &self,
        left: &str,
        right: &str,
        paths: &[PathBuf],
    ) -> Result<Vec<PathBuf>> {
        let mut args = vec![
            OsString::from("diff"),
            OsString::from("--name-only"),
            OsString::from("-z"),
            OsString::from(format!("{left}...{right}")),
            OsString::from("--"),
        ];
        args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
        let output = self.invoke_os(Class::Query, args)?;
        Ok(output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| PathBuf::from(OsString::from_vec(part.to_vec())))
            .collect())
    }

    /// Reads the branch at spawn; detached HEAD is `None`.
    pub fn head_branch(&self, tree: &Path) -> Result<Option<String>> {
        let output = self.invoke_status_in(
            tree,
            Class::Query,
            proc::os_args(&["symbolic-ref", "--short", "-q", "HEAD"]),
        )?;
        match output.child.code {
            Some(0) => Ok(Some(text(&output.stdout))),
            Some(1) => Ok(None),
            _ => Err(command_failed(Class::Query, &[], &output)),
        }
    }

    /// Reads origin's URL; an absent origin is `None`.
    pub fn origin_url(&self) -> Result<Option<String>> {
        let output = self.invoke_status(Class::Query, &["config", "--get", "remote.origin.url"])?;
        match output.child.code {
            Some(0) => Ok(Some(text(&output.stdout))),
            Some(1) => Ok(None),
            _ => Err(command_failed(Class::Query, &[], &output)),
        }
    }

    /// Checks one ref without treating absence as subprocess failure.
    pub fn ref_exists(&self, reference: &str) -> Result<bool> {
        let output = self.invoke_status(
            Class::Query,
            &["rev-parse", "--verify", "--quiet", reference],
        )?;
        match output.child.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(command_failed(Class::Query, &[], &output)),
        }
    }

    /// Resolves a revision to a commit object without treating absence as a
    /// subprocess failure.
    pub fn resolve_commit(&self, revision: &str) -> Result<Option<String>> {
        let expression = format!("{revision}^{{commit}}");
        let output = self.invoke_status(
            Class::Query,
            &["rev-parse", "--verify", "--quiet", &expression],
        )?;
        match output.child.code {
            Some(0) => Ok(Some(text(&output.stdout))),
            Some(1) => Ok(None),
            _ => Err(command_failed(Class::Query, &[], &output)),
        }
    }

    /// Reads one branch's upstream short name and gone marker in one query.
    ///
    /// Git refnames cannot contain the unit separator, so `%1f` splits the
    /// two fields unambiguously even for exotic branch names.
    pub fn upstream_info(&self, branch: &str) -> Result<UpstreamInfo> {
        let value = self.text(
            Class::Query,
            &[
                "for-each-ref",
                "--format=%(upstream:short)%1f%(upstream:track)",
                &format!("refs/heads/{branch}"),
            ],
        )?;
        let (name, track) = value.split_once('\u{1f}').unwrap_or((value.as_str(), ""));
        Ok(UpstreamInfo {
            name: (!name.is_empty()).then(|| name.to_owned()),
            gone: track == "[gone]",
        })
    }

    /// Reads the configured upstream short name for one local branch.
    pub fn upstream(&self, branch: &str) -> Result<Option<String>> {
        let value = self.text(
            Class::Query,
            &[
                "for-each-ref",
                "--format=%(upstream:short)",
                &format!("refs/heads/{branch}"),
            ],
        )?;
        Ok((!value.is_empty()).then_some(value))
    }

    /// Reads HEAD's object id.
    pub fn head_oid(&self) -> Result<String> {
        self.text(Class::Query, &["rev-parse", "HEAD"])
    }

    /// Checks `merge-base --is-ancestor` without treating false as failure.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let output = self.invoke_status(
            Class::Query,
            &["merge-base", "--is-ancestor", ancestor, descendant],
        )?;
        match output.child.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(command_failed(Class::Query, &[], &output)),
        }
    }

    /// Reads the stable upstream tracking marker for one local branch.
    pub fn upstream_track(&self, branch: &str) -> Result<String> {
        self.text(
            Class::Query,
            &[
                "for-each-ref",
                "--format=%(upstream:track)",
                &format!("refs/heads/{branch}"),
            ],
        )
    }

    /// Lists remote branches containing the given revision.
    pub fn remote_branches_containing(&self, revision: &str) -> Result<Vec<String>> {
        let output = self.invoke(
            Class::Query,
            &[
                "branch",
                "-r",
                "--format=%(refname:short)",
                "--contains",
                revision,
            ],
        )?;
        Ok(lines(&output.stdout))
    }

    /// The repository-wide part of the default-branch chain: origin/HEAD →
    /// main/master/trunk. `None` means the caller must apply the final,
    /// per-worktree HEAD fallback — that step depends on which checkout is
    /// asking and must not be shared between trees.
    pub fn default_branch_shared(&self) -> Result<Option<String>> {
        let origin = self.invoke_status(
            Class::Query,
            &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
        )?;
        if origin.child.code == Some(0) {
            if let Some(branch) = text(&origin.stdout).strip_prefix("origin/") {
                return Ok(Some(branch.to_owned()));
            }
        }
        for branch in ["main", "master", "trunk"] {
            if self.ref_exists(&format!("refs/heads/{branch}"))? {
                return Ok(Some(branch.to_owned()));
            }
        }
        Ok(None)
    }

    /// Implements the origin/HEAD → main/master/trunk → HEAD default-branch chain.
    pub fn default_branch(&self) -> Result<String> {
        if let Some(branch) = self.default_branch_shared()? {
            return Ok(branch);
        }
        Ok(self
            .head_branch(&self.repo)?
            .unwrap_or_else(|| "main".to_owned()))
    }

    /// Returns a worktree path holding the named local branch, if any.
    pub fn branch_holder(&self, branch: &str) -> Result<Option<PathBuf>> {
        Ok(self
            .worktrees()?
            .into_iter()
            .find(|worktree| worktree.branch.as_deref() == Some(branch))
            .map(|worktree| worktree.path))
    }

    /// Deletes a local branch after orchestration has made the pure safety decision.
    pub fn branch_delete(&self, branch: &str, force: bool) -> Result<()> {
        let flag = if force { "-D" } else { "-d" };
        self.invoke(Class::Worktree, &["branch", flag, branch])?;
        Ok(())
    }

    fn text(&self, class: Class, args: &[&str]) -> Result<String> {
        self.invoke(class, args).map(|output| text(&output.stdout))
    }

    fn invoke(&self, class: Class, args: &[&str]) -> Result<ProcessOutput> {
        self.invoke_os(class, proc::os_args(args))
    }

    fn invoke_os(&self, class: Class, args: Vec<OsString>) -> Result<ProcessOutput> {
        self.invoke_in(&self.repo, class, args)
    }

    fn invoke_in(&self, cwd: &Path, class: Class, args: Vec<OsString>) -> Result<ProcessOutput> {
        let output = self.invoke_status_in(cwd, class, args.clone())?;
        ensure_success(class, &args, output)
    }

    fn invoke_status(&self, class: Class, args: &[&str]) -> Result<ProcessOutput> {
        self.invoke_status_in(&self.repo, class, proc::os_args(args))
    }

    fn invoke_status_in(
        &self,
        cwd: &Path,
        class: Class,
        args: Vec<OsString>,
    ) -> Result<ProcessOutput> {
        self.invoke_request(cwd, class, args, false, &[])
    }

    fn invoke_request(
        &self,
        cwd: &Path,
        class: Class,
        args: Vec<OsString>,
        optional_locks: bool,
        extra_env: &[(String, String)],
    ) -> Result<ProcessOutput> {
        let mut request = CommandRequest {
            program: self.program.clone(),
            args,
            cwd: Some(cwd.to_path_buf()),
            env: [
                ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
                ("LC_ALL".to_owned(), "C".to_owned()),
            ]
            .into_iter()
            .collect(),
            remove_env: git_environment_to_clear(),
            clear_env: false,
            nice: None,
        };
        request
            .env
            .insert("GIT_LITERAL_PATHSPECS".to_owned(), "1".to_owned());
        if class == Class::Query && !optional_locks {
            request
                .env
                .insert("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned());
        }
        request.env.extend(extra_env.iter().cloned());
        proc::capture_op(
            &request,
            self.deadlines.for_class(class),
            Some(&git_op(&request.args)),
        )
        .map_err(|error| {
            if error.code.0 == "SPAWN_FAILED" {
                CoreError::new(
                    ExitClass::External,
                    "GIT_FAILED",
                    format!("could not spawn git: {}", error.message),
                    "install git and verify that it is executable",
                )
            } else {
                error
            }
        })
    }
}

/// Names a git invocation by its leading subcommand words for the timing log.
/// wt composes every one of these argument lists, so none of it is user text.
fn git_op(args: &[OsString]) -> String {
    args.iter()
        .take_while(|arg| !arg.to_string_lossy().starts_with('-'))
        .take(2)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Executes one bounded `git clone` without consulting repository state.
pub fn clone(
    program: impl Into<OsString>,
    url: &str,
    path: &Path,
    deadline: Duration,
) -> Result<()> {
    let mut request = CommandRequest::new(program);
    request.args = vec![
        OsString::from("clone"),
        OsString::from("--"),
        OsString::from(url),
        path.as_os_str().to_owned(),
    ];
    request.env = [
        ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
        ("LC_ALL".to_owned(), "C".to_owned()),
    ]
    .into_iter()
    .collect();
    request.remove_env = git_environment_to_clear();
    let output = proc::capture(&request, deadline)?;
    ensure_success(Class::Clone, &request.args, output).map(|_| ())
}

/// Creates a bare hub at `hub` for `url` (§11.6): an ordinary remote with
/// the standard fetch refspec, every origin branch under
/// `refs/remotes/origin/`, and one local branch — the default, which HEAD
/// names — so the hub looks like a clone that has checked nothing out.
/// Returns the default branch's name.
pub fn clone_hub(
    program: impl Into<OsString>,
    url: &str,
    hub: &Path,
    deadline: Duration,
) -> Result<String> {
    let program = program.into();
    let run = |args: &[&str], class: Class, timeout: Duration| -> Result<ProcessOutput> {
        let mut request = CommandRequest::new(program.clone());
        request.args = proc::os_args(args);
        request.env = [
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ]
        .into_iter()
        .collect();
        request.remove_env = git_environment_to_clear();
        let output = proc::capture_op(&request, timeout, Some(&git_op(&request.args)))?;
        ensure_success(class, &request.args, output)
    };
    let hub_arg = hub.to_string_lossy().into_owned();
    let query = Duration::from_secs(30);
    run(
        &["init", "--quiet", "--bare", "--", &hub_arg],
        Class::Query,
        query,
    )?;
    // From here a failure — a remote that cannot be reached, a fetch past
    // its deadline — must not leave a half-made hub that a retry would
    // take for a finished one.
    let populate = || -> Result<String> {
        run(
            &["-C", &hub_arg, "remote", "add", "origin", "--", url],
            Class::Query,
            query,
        )?;
        run(
            &["-C", &hub_arg, "fetch", "--quiet", "origin"],
            Class::Clone,
            deadline,
        )?;
        run(
            &["-C", &hub_arg, "remote", "set-head", "origin", "--auto"],
            Class::Fetch,
            deadline,
        )?;
        let head = run(
            &[
                "-C",
                &hub_arg,
                "symbolic-ref",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
            Class::Query,
            query,
        )?;
        let default = text(&head.stdout)
            .strip_prefix("origin/")
            .map(str::to_owned)
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::External,
                    "GIT_FAILED",
                    format!("origin at {url} names no default branch"),
                    "make sure the remote has a HEAD branch",
                )
            })?;
        run(
            &[
                "-C",
                &hub_arg,
                "branch",
                "--quiet",
                "--",
                &default,
                &format!("origin/{default}"),
            ],
            Class::Query,
            query,
        )?;
        run(
            &[
                "-C",
                &hub_arg,
                "symbolic-ref",
                "HEAD",
                &format!("refs/heads/{default}"),
            ],
            Class::Query,
            query,
        )?;
        Ok(default)
    };
    match populate() {
        Ok(default) => Ok(default),
        Err(error) => {
            let _ = std::fs::remove_dir_all(hub);
            Err(error)
        }
    }
}

/// Parses the forward-compatible `worktree list --porcelain` format.
pub fn parse_worktree_porcelain(input: &str) -> Result<Vec<Worktree>> {
    let mut output = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in input.lines() {
        if line.is_empty() {
            if let Some(worktree) = current.take() {
                output.push(worktree);
            }
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "worktree" => {
                if let Some(worktree) = current.take() {
                    output.push(worktree);
                }
                current = Some(Worktree {
                    path: PathBuf::from(value),
                    head: String::new(),
                    branch: None,
                    bare: false,
                    detached: false,
                    locked: None,
                    prunable: None,
                });
            }
            "HEAD" => set_worktree(&mut current, |worktree| worktree.head = value.to_owned())?,
            "branch" => set_worktree(&mut current, |worktree| {
                worktree.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_owned(),
                );
            })?,
            "bare" => set_worktree(&mut current, |worktree| worktree.bare = true)?,
            "detached" => set_worktree(&mut current, |worktree| worktree.detached = true)?,
            "locked" => set_worktree(&mut current, |worktree| {
                worktree.locked = Some(value.to_owned());
            })?,
            "prunable" => set_worktree(&mut current, |worktree| {
                worktree.prunable = Some(value.to_owned());
            })?,
            _ => {}
        }
    }
    if let Some(worktree) = current {
        output.push(worktree);
    }
    Ok(output)
}

/// Parses NUL-delimited porcelain-v1 status without interpreting domain state.
pub fn parse_status_porcelain(input: &[u8]) -> Result<Vec<StatusEntry>> {
    let fields = input.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut index = 0;
    let mut entries = Vec::new();
    while index < fields.len() && !fields[index].is_empty() {
        let field = fields[index];
        if field.len() < 4 || field[2] != b' ' {
            return Err(format_error("git status porcelain entry is malformed"));
        }
        let status = (field[0] as char, field[1] as char);
        let path = PathBuf::from(OsString::from_vec(field[3..].to_vec()));
        index += 1;
        let original_path = if matches!(status.0, 'R' | 'C') || matches!(status.1, 'R' | 'C') {
            let Some(original) = fields.get(index).filter(|value| !value.is_empty()) else {
                return Err(format_error("git rename status lacks its original path"));
            };
            index += 1;
            Some(PathBuf::from(OsString::from_vec(original.to_vec())))
        } else {
            None
        };
        entries.push(StatusEntry {
            index: status.0,
            worktree: status.1,
            path,
            original_path,
        });
    }
    Ok(entries)
}

fn set_worktree(current: &mut Option<Worktree>, update: impl FnOnce(&mut Worktree)) -> Result<()> {
    let Some(worktree) = current.as_mut() else {
        return Err(format_error(
            "git worktree field precedes its worktree path",
        ));
    };
    update(worktree);
    Ok(())
}

fn ensure_success(class: Class, args: &[OsString], output: ProcessOutput) -> Result<ProcessOutput> {
    if output.timed_out {
        return Err(CoreError::new(
            ExitClass::Timeout,
            "TIMEOUT",
            format!("git {:?} command timed out", class),
            "retry the operation or raise the matching git timeout",
        ));
    }
    if !output.success() {
        return Err(command_failed(class, args, &output));
    }
    Ok(output)
}

fn command_failed(class: Class, args: &[OsString], output: &ProcessOutput) -> CoreError {
    let code = match class {
        Class::Fetch => "FETCH_FAILED",
        _ => "GIT_FAILED",
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    CoreError::new(
        ExitClass::External,
        code,
        format!(
            "git {} failed with status {}: {}",
            args.iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            output.mapped_exit(),
            stderr.trim()
        ),
        "inspect git's error, fix the repository or remote, and retry",
    )
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim_end().to_owned()
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn parse_error(context: &str, error: std::num::ParseIntError) -> CoreError {
    CoreError::new(
        ExitClass::External,
        "GIT_FAILED",
        format!("could not parse git {context}: {error}"),
        "verify the installed git emits standard machine-readable output",
    )
}

fn format_error(message: &str) -> CoreError {
    CoreError::new(
        ExitClass::External,
        "GIT_FAILED",
        message,
        "verify the installed git emits standard porcelain output",
    )
}

fn git_io(context: &str, error: std::io::Error) -> CoreError {
    CoreError::new(
        ExitClass::Internal,
        "IO_FAILED",
        format!("{context}: {error}"),
        "retry the operation and inspect repository permissions if it repeats",
    )
}

fn git_environment_to_clear() -> Vec<OsString> {
    [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn resolved_duration(value: Option<&str>, key: &str) -> Result<Duration> {
    let millis = value.and_then(duration_millis).ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "SETTINGS_INVALID",
            format!("{key} has no resolved valid duration"),
            "fix `$WT_HOME/config.toml` and reload settings",
        )
    })?;
    Ok(Duration::from_millis(millis))
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Instant;

    use tempfile::tempdir;

    use super::*;

    fn repo() -> (tempfile::TempDir, Git) {
        let dir = tempdir().unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.email", "wt@example.test"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.name", "wt test"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        std::fs::write(dir.path().join("tracked"), "one").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "initial"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        let git = Git::open_validated(dir.path(), Deadlines::default()).unwrap();
        (dir, git)
    }

    #[test]
    fn parses_worktree_and_status_porcelain() {
        let worktrees = parse_worktree_porcelain(
            "worktree /repo/a b\nHEAD abc\nbranch refs/heads/main\n\nworktree /tmp/d\nHEAD def\ndetached\n",
        )
        .unwrap();
        assert_eq!(worktrees[0].path, Path::new("/repo/a b"));
        assert!(worktrees[1].detached);
        let status = parse_status_porcelain(b" M tracked\0?? new file\0").unwrap();
        assert_eq!(status.len(), 2);
        assert_eq!(status[1].path, Path::new("new file"));
    }

    #[test]
    fn temp_repo_queries_cover_hash_tracked_status_and_history() {
        let (dir, git) = repo();
        let hashes = git
            .hash_object(&[PathBuf::from("missing"), PathBuf::from("tracked")])
            .unwrap();
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0], "<missing>");
        assert_ne!(hashes[1], "<missing>");
        assert_eq!(
            git.tracked_paths(
                dir.path(),
                &[PathBuf::from("tracked"), PathBuf::from("new")]
            )
            .unwrap(),
            BTreeSet::from([PathBuf::from("tracked")])
        );
        std::fs::write(dir.path().join("tracked"), "two").unwrap();
        std::fs::write(dir.path().join("new"), "new").unwrap();
        assert_eq!(git.status_porcelain(dir.path()).unwrap().len(), 2);
        assert_eq!(git.rev_list_count("HEAD").unwrap(), 1);
        assert_eq!(
            git.ahead_behind("HEAD", "HEAD").unwrap(),
            AheadBehind::default()
        );
        assert_eq!(
            git.default_branch().unwrap(),
            git.head_branch(dir.path()).unwrap().unwrap()
        );
        assert!(Command::new("git")
            .args(["add", "tracked"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "second"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        assert_eq!(
            git.diff_name_only("HEAD^", "HEAD", &[PathBuf::from("tracked")])
                .unwrap(),
            [PathBuf::from("tracked")]
        );
    }

    #[test]
    fn temp_repo_worktree_mutations_round_trip() {
        let (_dir, git) = repo();
        let worktree_parent = tempdir().unwrap();
        let path = worktree_parent.path().join("linked");
        git.worktree_add(
            &path,
            &AddSpec::NewBranch {
                name: "linked".into(),
                start: "HEAD".into(),
                track: false,
            },
        )
        .unwrap();
        let canonical_path = path.canonicalize().unwrap();
        assert!(git
            .worktrees()
            .unwrap()
            .iter()
            .any(|item| item.path == canonical_path));
        git.worktree_repair(std::slice::from_ref(&path)).unwrap();
        git.worktree_remove(&path, true).unwrap();
        git.worktree_prune().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn default_branch_and_url_helpers_keep_borrowed_behavior() {
        assert_eq!(
            normalize_url("git@github.com:o/r.git"),
            Some(("github.com".into(), "o/r".into()))
        );
        assert_eq!(
            pr_refspec("gitlab.com", 9)[0].0,
            "refs/merge-requests/9/head"
        );
    }

    #[test]
    fn fetch_class_uses_its_own_bounded_deadline() {
        let dir = tempdir().unwrap();
        let program = dir.path().join("git-stub");
        crate::stub::write(
            &program,
            "#!/bin/sh\nif [ \"$1\" = rev-parse ]; then printf 'true\\n'; exit 0; fi\nif [ \"$1\" = fetch ]; then sleep 5; fi\n",
        );
        let deadlines = Deadlines {
            query: Duration::from_secs(1),
            fetch: Duration::from_millis(20),
            ..Deadlines::default()
        };
        let git = Git::open_with_program(dir.path(), program, deadlines).unwrap();
        let started = Instant::now();
        assert_eq!(git.fetch("origin", "main").unwrap_err().code.0, "TIMEOUT");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn open_is_query_free_and_core_controls_branch_tracking() {
        let dir = tempdir().unwrap();
        let program = dir.path().join("git-stub");
        let record = dir.path().join("argv");
        crate::stub::write(
            &program,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                record.display()
            ),
        );

        let git = Git::open_with_program(dir.path(), program, Deadlines::default()).unwrap();
        assert!(!record.exists(), "opening a handle must not query git");
        git.worktree_add(
            &dir.path().join("tracked"),
            &AddSpec::NewBranch {
                name: "tracked".into(),
                start: "origin/main".into(),
                track: true,
            },
        )
        .unwrap();
        assert!(!std::fs::read_to_string(&record)
            .unwrap()
            .lines()
            .any(|argument| argument == "--no-track"));

        git.worktree_add(
            &dir.path().join("untracked"),
            &AddSpec::NewBranch {
                name: "untracked".into(),
                start: "HEAD".into(),
                track: false,
            },
        )
        .unwrap();
        assert!(std::fs::read_to_string(record)
            .unwrap()
            .lines()
            .any(|argument| argument == "--no-track"));
    }
}
