//! Creating and removing trees, and the hooks that run around them.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::config::Config;
use crate::error::{Error, Result, io_at};
use crate::run::{Outcome, RunHook};
use crate::{Init, Tree, Wt, git, names, state, template};

/// A tree's lifecycle hooks, from `[lifecycle]` in its config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    /// After the tree is made.
    Create,
    /// Bring up what the tree runs. Must be safe to repeat.
    Start,
    /// Bring it down, keeping data. Must be safe to repeat.
    Stop,
    /// After `Stop`, before the tree is removed: remove what belongs to it
    /// alone. Must cope with nothing having been started.
    Delete,
}

impl Hook {
    pub fn name(self) -> &'static str {
        match self {
            Hook::Create => "create",
            Hook::Start => "start",
            Hook::Stop => "stop",
            Hook::Delete => "delete",
        }
    }
}

/// How the tree's `create` hook last went, from `.wt/create.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateStatus {
    NotRun,
    /// Running, or interrupted before it could record an exit code.
    Running,
    Succeeded,
    Failed(i32),
}

/// What a tree will be called and where it goes: the input to
/// [`Wt::plan_new`].
#[derive(Clone, Debug)]
pub struct PlanRequest {
    /// The repository's label. Defaults to the repository containing the
    /// directory the plan is made from.
    pub label: Option<String>,
    pub name: String,
    /// The branch, instead of one rendered from the config's `branch`
    /// templates (or the tree's name when there are none). A branch asked
    /// for by name that already exists is checked out.
    pub branch: Option<String>,
    /// Values for the `branch` templates.
    pub vars: BTreeMap<String, String>,
}

impl PlanRequest {
    pub fn named(name: impl Into<String>) -> PlanRequest {
        PlanRequest {
            label: None,
            name: name.into(),
            branch: None,
            vars: BTreeMap::new(),
        }
    }
}

/// A tree that can be made, from [`Wt::plan`] or [`Wt::plan_new`], checked
/// against what is on this machine without touching the network. Give it to
/// [`Wt::new_tree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub label: String,
    pub name: String,
    /// Where the tree will be.
    pub path: PathBuf,
    pub branch: String,
    /// The repository's main checkout.
    pub main: PathBuf,
    /// The branch was asked for by name rather than rendered.
    named_branch: bool,
}

/// How to make a planned tree with `git worktree add`.
#[derive(Clone, Debug)]
pub struct NewTree {
    /// What to start from instead of `origin/<default branch>`.
    pub from: Option<String>,
    /// Fetch the default branch first.
    pub fetch: bool,
    /// Fail with [`crate::Error::Taken`] when the branch already exists,
    /// locally or (after a fetch) on `origin`, instead of checking it out.
    pub fresh_branch: bool,
    /// Recorded in `.wt/owner`, for [`Tree::owner`]: who made the tree, as
    /// an opaque string.
    pub owner: Option<String>,
}

impl Default for NewTree {
    fn default() -> NewTree {
        NewTree {
            from: None,
            fetch: true,
            fresh_branch: false,
            owner: None,
        }
    }
}

#[derive(Debug)]
pub struct Created {
    pub tree: Tree,
    /// Things that went wrong without stopping the tree being made.
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Remove {
    /// Remove even with uncommitted changes or untracked files.
    pub ignore_dirty: bool,
    /// Remove even while something holds the tree.
    pub ignore_holders: bool,
    /// Leave the worktree on disk and only stop managing it.
    pub keep: bool,
    /// Delete the tree's branch too when it has no commits of its own: when
    /// its tip is reachable from another ref (other than its own upstream).
    /// A branch with commits of its own is always kept.
    pub delete_empty_branch: bool,
    /// How the `stop` and `delete` hooks run.
    pub hooks: RunHook,
}

#[derive(Debug)]
pub struct Removed {
    /// The branch the tree had checked out.
    pub branch: Option<String>,
    /// Whether that branch was deleted, by [`Remove::delete_empty_branch`].
    pub branch_deleted: bool,
    /// Things that went wrong without stopping the removal: a failed hook,
    /// a branch that couldn't be deleted.
    pub warnings: Vec<String>,
}

/// A fetch that hangs shouldn't hold up making a tree: the last fetched copy
/// of the default branch is a fine place to start.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs the `create` hook, recording `running` and then its exit code in
/// `.wt/create.status`, and its output in `.wt/create.log`. Arguments: the
/// status file, the log, the hook, `1` to copy the output to stdout too,
/// and then a command to run first (its failure is the hook's).
///
/// Whoever runs it hands it the lifecycle lock as fd 9, and the hook and
/// the first command run with that closed, so build servers the hook leaves
/// behind never hold it. Without `tee` the output goes to the log by
/// redirection, not through a pipe, because something left holding the
/// hook's stdout would keep `tee`, and so the wrapper and its lock, alive.
///
/// The status is replaced by a rename, never rewritten in place, because a
/// reader that caught it empty would take the hook for finished.
const CREATE_WRAPPER: &str = r#"trap : INT TERM HUP
status="$1"; log="$2"; hook="$3"; tee="$4"; shift 4
record() { printf '%s' "$1" > "$status.new" && mv -f "$status.new" "$status"; }
record running
: > "$log"
code=0
if [ $# -gt 0 ]; then "$@" >> "$log" 2>&1 9>&- || code=$?; fi
if [ "$code" = 0 ]; then
  if [ "$tee" = 1 ]; then
    { sh -c "$hook" 2>&1 9>&-; echo $? > "$status.exit"; } | tee -a "$log" 9>&-
    code=$(cat "$status.exit"); rm -f "$status.exit"
  else
    sh -c "$hook" >> "$log" 2>&1 9>&-; code=$?
  fi
fi
record "$code"
exit "$code""#;

/// Runs a `start`, `stop` or `delete` hook. Arguments: the hook, and the
/// seconds after which the wrapper kills it.
///
/// Whoever runs it hands it the lifecycle lock as fd 9 and, for a stop or a
/// removal, the exclusive hold as fd 8, so both last until the hook ends
/// even if that process dies; the hook runs with them closed, so services it
/// leaves running never hold them. The signals are trapped, not ignored, so
/// the hook still gets them and the wrapper stays until it has ended.
///
/// The watchdog is what enforces the time limit once the process that ran
/// the hook is gone, which would otherwise leave a hung hook holding the
/// locks for good. It gives the runner's own, exact limit time to act first:
/// it fires a little later, sending SIGTERM and then SIGKILL to the process
/// group, wrapper included. It holds no lock and none of the hook's output.
const HOOK_WRAPPER: &str = r#"trap : INT TERM HUP
watchdog=
if [ -n "$2" ]; then
  (
    trap '' INT HUP
    trap 'kill "$s" 2>/dev/null; exit 0' TERM
    sleep "$2" & s=$!
    wait "$s"
    trap '' TERM
    kill -TERM 0
    sleep 3
    kill -KILL 0
  ) < /dev/null > /dev/null 2>&1 8>&- 9>&- &
  watchdog=$!
fi
sh -c "$1" 8>&- 9>&-
code=$?
[ -n "$watchdog" ] && kill "$watchdog" 2>/dev/null
exit "$code""#;

/// How much later than the time limit a hook's own wrapper kills it: after
/// the runner's SIGTERM, its grace and some slack.
const WATCHDOG_SLACK: Duration = Duration::from_secs(5);

impl Wt {
    /// Plans a tree named `name` in the repository containing `repository`,
    /// with its branch from the config's templates, answering locally and
    /// quickly. `None` when wt isn't configured for the repository: it has
    /// no `.wt.toml` in its main checkout and no user-layer config.
    ///
    /// Fails with [`crate::Error::Taken`] when the name, the path or the
    /// local branch is already in use, so a caller choosing names can try
    /// another.
    pub fn plan(&self, repository: &Path, name: &str) -> Result<Option<Planned>> {
        let (label, main) = self.find_repository(None, repository)?;
        if Config::source(&self.home, &label, &main).is_none() {
            return Ok(None);
        }
        let planned = self.plan_in(label, main, &PlanRequest::named(name))?;
        if git::ref_exists(&planned.main, &format!("refs/heads/{}", planned.branch)) {
            return Err(Error::Taken(format!(
                "branch `{}` already exists",
                planned.branch
            )));
        }
        Ok(Some(planned))
    }

    /// Plans a tree as `wt new` does: with or without a config, and leaving
    /// an existing branch to be checked out.
    pub fn plan_new(&self, cwd: &Path, request: &PlanRequest) -> Result<Planned> {
        let (label, main) = self.find_repository(request.label.as_deref(), cwd)?;
        self.plan_in(label, main, request)
    }

    fn plan_in(&self, label: String, main: PathBuf, request: &PlanRequest) -> Result<Planned> {
        let name = request.name.clone();
        if !names::valid(&name) {
            return Err(Error::Conflict(format!("`{name}` can't be a tree name")));
        }
        let path = self.settings.trees_dir.join(&label).join(&name);
        self.check_free(&label, &name, &path)?;
        let config = Config::load(&self.home, &label, &main)?;
        let branch = branch_for(request, &config, &label)?;
        git::run(&main, &["check-ref-format", "--branch", &branch])
            .map_err(|_| Error::Conflict(format!("`{branch}` isn't a valid branch name")))?;
        Ok(Planned {
            label,
            name,
            path,
            branch,
            main,
            named_branch: request.branch.is_some(),
        })
    }

    fn check_free(&self, label: &str, name: &str, path: &Path) -> Result<()> {
        if let Some(other) = state::read(&self.home)?.tree(label, name) {
            return Err(Error::Taken(format!(
                "{label}/{name} already exists at {}",
                other.path.display()
            )));
        }
        if path.exists() {
            return Err(Error::Taken(format!("{} already exists", path.display())));
        }
        Ok(())
    }

    /// Makes the planned tree under the trees directory and initialises it.
    /// It starts from `origin/<default branch>` (after a fetch that falls
    /// back to the last fetched copy) unless told otherwise; the main
    /// checkout and its branches are never touched. Doesn't copy inputs or
    /// run the `create` hook: see [`Tree::copy_inputs`] and
    /// [`Tree::spawn_create`].
    pub fn new_tree(&self, plan: &Planned, request: NewTree) -> Result<Created> {
        let Planned {
            label,
            name,
            path,
            branch,
            main,
            named_branch,
        } = plan.clone();
        // Anything could have happened since the plan was made.
        self.check_free(&label, &name, &path)?;
        state::update(&self.home, |state| crate::register(state, &label, &main))?;

        let mut warnings = Vec::new();
        let local = git::ref_exists(&main, &format!("refs/heads/{branch}"));
        if local && request.fresh_branch {
            return Err(Error::Taken(format!("branch `{branch}` already exists")));
        }
        let remote_ref = format!("refs/remotes/origin/{branch}");
        let origin = request.fetch && git::has_origin(&main);
        // A branch asked for by name may be someone else's on origin: fetch
        // it, so that it is checked out rather than started afresh. It may
        // equally be new, so a failed fetch says nothing. A fresh branch is
        // fetched to make sure nobody has it.
        let fetch_branch = origin && !local && (named_branch || request.fresh_branch);
        // The default branch is fetched alongside, before knowing whether
        // the tree starts from it: each fetch is a round trip to origin of
        // its own, over ssh most of the time a new tree takes, and a tree
        // that starts afresh needs both. One fetch can't ask for both,
        // since it fails outright when the branch isn't on origin, which
        // is the usual case. They update different refs and write no
        // FETCH_HEAD, so they don't meet.
        let default = (origin && !local && request.from.is_none())
            .then(|| git::default_branch(&main))
            .flatten();
        let fetched_default = std::thread::scope(|scope| {
            let fetching_default = default.as_deref().map(|default| {
                scope.spawn(|| {
                    git::run_timeout(
                        &main,
                        &[
                            "fetch",
                            "--quiet",
                            "--no-write-fetch-head",
                            "origin",
                            default,
                        ],
                        FETCH_TIMEOUT,
                    )
                })
            });
            if fetch_branch {
                let _ = git::run_timeout(
                    &main,
                    &[
                        "fetch",
                        "--quiet",
                        "--no-write-fetch-head",
                        "origin",
                        &format!("+refs/heads/{branch}:{remote_ref}"),
                    ],
                    FETCH_TIMEOUT,
                );
            }
            fetching_default.map(|fetching| fetching.join().expect("a fetch doesn't panic"))
        });
        let remote = !local && git::ref_exists(&main, &remote_ref);
        if remote && request.fresh_branch {
            return Err(Error::Taken(format!(
                "branch `{branch}` already exists on origin"
            )));
        }
        if (local || remote) && request.from.is_some() {
            return Err(Error::Conflict(format!(
                "branch `{branch}` already exists, so there's nothing to start it --from"
            )));
        }

        let parent = path.parent().expect("trees live in a directory");
        fs::create_dir_all(parent).map_err(io_at("creating", parent))?;
        let path_arg = path.to_string_lossy();
        let origin_branch = format!("origin/{branch}");
        let start = match &request.from {
            Some(from) => from.clone(),
            None if local || remote => String::new(),
            None => start_point(&main, default, fetched_default, &mut warnings),
        };
        // checkout.workers=0 checks files out in parallel, one worker per
        // core: on large repositories that is several times faster.
        let mut args = vec!["-c", "checkout.workers=0", "worktree", "add", "--quiet"];
        if local {
            args.extend([&*path_arg, &branch]);
        } else if remote {
            args.extend(["--track", "-b", &branch, &*path_arg, &origin_branch]);
        } else {
            args.extend(["--no-track", "-b", &branch, &*path_arg, &start]);
        }
        git::run(&main, &args)?;

        let init = Init {
            label: Some(label),
            name: Some(name),
        };
        let made = self.init(&path, init).and_then(|tree| {
            if let Some(owner) = &request.owner {
                tree.set_owner(owner)?;
            }
            Ok(tree)
        });
        match made {
            Ok(tree) => Ok(Created { tree, warnings }),
            Err(error) => {
                let _ = git::run(&main, &["worktree", "remove", "--force", &path_arg]);
                if !local {
                    let _ = git::run(&main, &["branch", "-D", &branch]);
                }
                let _ = state::update(&self.home, |state| {
                    state.trees.retain(|other| other.path != path);
                    Ok(())
                });
                Err(error)
            }
        }
    }

    /// Refuses a removal [`Wt::remove`] would refuse, without removing
    /// anything: the main checkout (unless `keep`), a tree something holds
    /// (unless `ignore_holders`), and one with uncommitted or untracked
    /// work (unless `ignore_dirty` or `keep`).
    pub fn check_remove(&self, tree: &Tree, options: Remove) -> Result<()> {
        self.check_main(tree, options)?;
        let _exclusive = self.check_holders(tree, options)?;
        self.check_dirty(tree, options)
    }

    fn check_main(&self, tree: &Tree, options: Remove) -> Result<()> {
        if !options.keep && tree.is_main() {
            return Err(Error::Conflict(format!(
                "{} is the main checkout; `--keep` stops managing it without deleting it",
                tree.id()
            )));
        }
        Ok(())
    }

    /// The exclusive hold, when the tree exists and nothing else holds it.
    /// Never waits for a holder, but does wait for a running hook, which
    /// may have the tree to itself (a stop) without being a holder.
    fn check_holders(&self, tree: &Tree, options: Remove) -> Result<Option<fs::File>> {
        if !tree.root.is_dir() {
            return Ok(None);
        }
        loop {
            if let Some(file) = tree.try_hold_exclusively()? {
                return Ok(Some(file));
            }
            // A running stop hook has the tree to itself, and that is not a
            // holder: perhaps a process that died mid-stop left it running.
            // Wait for it, as for any running hook, and look again.
            if tree.hook_running() {
                drop(tree.lock_lifecycle(options.hooks.forward)?);
                continue;
            }
            return if options.ignore_holders {
                Ok(None)
            } else {
                Err(Error::Held(format!(
                    "{} is in use: a `wt shell`, a `wt exec` or another program holds it; \
                     pass --force to remove it anyway",
                    tree.id()
                )))
            };
        }
    }

    fn check_dirty(&self, tree: &Tree, options: Remove) -> Result<()> {
        if !options.ignore_dirty
            && !options.keep
            && tree.root.is_dir()
            && git::is_dirty(&tree.root)?
        {
            return Err(Error::Conflict(format!(
                "{} has uncommitted changes or untracked files; commit them, or pass --force",
                tree.id()
            )));
        }
        Ok(())
    }

    /// Removes a tree: runs its `stop` and `delete` hooks, then removes its
    /// worktree (unless `keep`) and stops managing it. Never waits for
    /// holders: a tree something holds is refused with
    /// [`crate::Error::Held`] unless `ignore_holders`, and one with
    /// uncommitted work with [`crate::Error::Conflict`] unless
    /// `ignore_dirty`. It does wait for a hook already running, such as a
    /// `create` still building. A failed hook is a warning, and removal goes
    /// on. The branch is kept unless `delete_empty_branch` and it has no
    /// commits of its own.
    pub fn remove(&self, tree: &Tree, options: Remove) -> Result<Removed> {
        self.check_main(tree, options)?;
        // Held to the end, so nobody starts holding a tree being removed.
        let exclusive = self.check_holders(tree, options)?;
        self.check_dirty(tree, options)?;
        let mut warnings = Vec::new();
        let branch = tree.branch();
        if tree.root.is_dir() {
            let lifecycle = tree.lock_lifecycle(options.hooks.forward)?;
            // A tree its stop hook already stopped, and nothing has started
            // or held since, needn't be stopped again.
            let hooks: &[Hook] = if tree.stopped() {
                &[Hook::Delete]
            } else {
                &[Hook::Stop, Hook::Delete]
            };
            for &hook in hooks {
                match tree.run_locked(hook, options.hooks, &lifecycle, exclusive.as_ref()) {
                    // A person asked to give up: nothing more is torn down.
                    Ok(ran) if matches!(ran.outcome, Outcome::Interrupted(_)) => {
                        return Err(Error::Cancelled);
                    }
                    Ok(ran) => {
                        let problem = if options.keep {
                            ran.problem()
                        } else {
                            ran.problem_with_output()
                        };
                        warnings.extend(problem);
                    }
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(error) => warnings.push(format!(
                        "couldn't run {}'s {} hook: {error}",
                        tree.id(),
                        hook.name()
                    )),
                }
            }
            if !options.keep {
                // Again, now: work may have been saved while removal waited
                // for a running hook, or by the stop and delete hooks.
                self.check_dirty(tree, options)?;
                // What is left is either clean or ignored (build output, and
                // the files wt itself rendered), so forcing is safe here.
                let path = tree.root.to_string_lossy();
                git::run(&tree.main, &["worktree", "remove", "--force", &path])?;
            }
        } else {
            warnings.push(format!(
                "{} is already gone from disk; not running its hooks",
                tree.id()
            ));
            let _ = git::run(&tree.main, &["worktree", "prune"]);
        }
        state::update(&self.home, |state| {
            state.trees.retain(|record| record.path != tree.root);
            Ok(())
        })?;
        let mut branch_deleted = false;
        if options.delete_empty_branch
            && !options.keep
            && let Some(branch) = &branch
        {
            match delete_if_empty(&tree.main, branch) {
                Ok(deleted) => branch_deleted = deleted,
                Err(error) => {
                    warnings.push(format!("couldn't delete branch {branch}: {error}"));
                }
            }
        }
        Ok(Removed {
            branch,
            branch_deleted,
            warnings,
        })
    }

    /// The label and main checkout of the repository named `label`, or of
    /// the one containing `cwd`, without registering it.
    fn find_repository(&self, label: Option<&str>, cwd: &Path) -> Result<(String, PathBuf)> {
        let state = state::read(&self.home)?;
        if let Some(label) = label {
            return match state.repos.get(label) {
                Some(main) => Ok((label.to_owned(), main.clone())),
                None => Err(Error::NotFound(format!(
                    "no repository is labelled `{label}`; run `wt new` inside it, or `wt init` in its main checkout"
                ))),
            };
        }
        let main = git::main_checkout(cwd).map_err(|_| {
            Error::NotFound(format!(
                "{} isn't inside a repository; name the tree as <label>/<name>",
                cwd.display()
            ))
        })?;
        let label = self.label_for(&main, None)?;
        match state.repos.get(&label) {
            Some(other) if *other != main => Err(Error::Conflict(format!(
                "the label `{label}` is already used by {}; run `wt init --label` in {} first",
                other.display(),
                main.display()
            ))),
            _ => Ok((label, main)),
        }
    }
}

/// Deletes `branch` when its tip is reachable from a ref other than itself
/// and its own upstream: then it has no commits of its own and deleting it
/// loses nothing. wt records no start point, and the base moves, so
/// reachability is the only test that works.
fn delete_if_empty(main: &Path, branch: &str) -> Result<bool> {
    let own = format!("refs/heads/{branch}");
    let tip = git::run(main, &["rev-parse", "--verify", "--quiet", &own])?;
    let upstream = git::run(main, &["for-each-ref", "--format=%(upstream)", &own])?;
    let containing = git::run(
        main,
        &["for-each-ref", "--format=%(refname)", "--contains", &tip],
    )?;
    let elsewhere = containing
        .lines()
        .any(|name| name != own && name != upstream && name != "refs/stash");
    if !elsewhere {
        return Ok(false);
    }
    git::run(main, &["branch", "-D", branch])?;
    Ok(true)
}

/// `origin/<default>` after fetching it, or the main checkout's HEAD for a
/// repository without a usable remote.
/// Where a fresh tree starts: origin's default branch, as fetched just now
/// or else as fetched last, or the main checkout's HEAD when there is no
/// origin or it doesn't say which branch is its default.
fn start_point(
    main: &Path,
    default: Option<String>,
    fetched: Option<Result<String>>,
    warnings: &mut Vec<String>,
) -> String {
    if !git::has_origin(main) {
        return "HEAD".to_owned();
    }
    let Some(default) = default.or_else(|| git::default_branch(main)) else {
        warnings.push(
            "couldn't tell origin's default branch; starting from the main checkout's HEAD"
                .to_owned(),
        );
        return "HEAD".to_owned();
    };
    if let Some(Err(error)) = fetched {
        warnings.push(format!(
            "couldn't fetch origin/{default} ({error}); starting from the copy fetched last"
        ));
    }
    format!("origin/{default}")
}

/// The branch for a new tree: as asked, or the first `branch` template whose
/// variables are all set, or the tree's name.
fn branch_for(request: &PlanRequest, config: &Config, label: &str) -> Result<String> {
    if let Some(branch) = &request.branch {
        return Ok(branch.clone());
    }
    if config.branch.is_empty() {
        return Ok(request.name.clone());
    }
    let mut vars = request.vars.clone();
    vars.insert("name".to_owned(), request.name.clone());
    vars.insert("label".to_owned(), label.to_owned());
    for candidate in &config.branch {
        let complete = template::variables(candidate)
            .iter()
            .all(|key| vars.get(key).is_some_and(|value| !value.is_empty()));
        if complete {
            return template::render(candidate, &vars, "branch");
        }
    }
    Err(Error::Config(format!(
        "no `branch` template has all its values set ({}); pass --branch, or --set the missing values",
        config.branch.join(", ")
    )))
}

impl Tree {
    /// The hook as a command ready to spawn: `sh -c` at the tree's root with
    /// its environment and `WT_IN_HOOK` set to the hook's name. `None` when
    /// the config doesn't declare it. `create` also records its progress
    /// for [`Tree::create_status`] and the `[bin]` wrappers, and copies its
    /// output (stderr merged into stdout) to [`Tree::create_log`].
    ///
    /// The command runs without wt's ordering and time limits; to have them,
    /// use [`Tree::run_hook`], [`Tree::spawn_create`] or
    /// [`Tree::stop_when_unheld`] instead.
    pub fn hook(&self, hook: Hook) -> Result<Option<Command>> {
        self.hook_command(hook, true, &[])
    }

    pub(crate) fn hook_command(
        &self,
        hook: Hook,
        tee: bool,
        before: &[OsString],
    ) -> Result<Option<Command>> {
        let config = self.config()?;
        let lifecycle = &config.lifecycle;
        let script = match hook {
            Hook::Create => &lifecycle.create,
            Hook::Start => &lifecycle.start,
            Hook::Stop => &lifecycle.stop,
            Hook::Delete => &lifecycle.delete,
        };
        let Some(script) = script else {
            return Ok(None);
        };
        let env = self.env()?;
        let mut command = Command::new("sh");
        if hook == Hook::Create {
            let directory = self.root.join(".wt");
            fs::create_dir_all(&directory).map_err(io_at("creating", &directory))?;
            command
                .arg("-c")
                .arg(CREATE_WRAPPER)
                .arg("wt-create")
                .arg(directory.join("create.status"))
                .arg(directory.join("create.log"))
                .arg(script)
                .arg(if tee { "1" } else { "0" })
                .args(before);
        } else {
            let watchdog = lifecycle
                .time_limit(hook)
                .map(|limit| format!("{:.3}", (limit + WATCHDOG_SLACK).as_secs_f64()))
                .unwrap_or_default();
            command
                .arg("-c")
                .arg(HOOK_WRAPPER)
                .arg(format!("wt-{}", hook.name()))
                .arg(script)
                .arg(watchdog);
        }
        env.apply(&mut command);
        // Tells `wt shell` and `wt exec` run from a hook not to hold the
        // tree: a stop hook's `wt exec` would otherwise wait on its own stop.
        command.env("WT_IN_HOOK", hook.name());
        command.current_dir(&self.root);
        Ok(Some(command))
    }

    /// Starts the `create` hook detached, in a session of its own, and
    /// returns at once; `None` when the config declares no `create` hook.
    /// The tree is marked creating first (see [`Tree::mark_creating`]).
    ///
    /// The hook holds the tree's lifecycle lock from before this returns
    /// until it finishes, whether or not this process is still alive, so a
    /// start, a stop or a removal queues behind the build. Its output goes
    /// to [`Tree::create_log`]. Waits first for any hook already running.
    /// Wait on the child (on a thread of its own, say) to reap it.
    pub fn spawn_create(&self) -> Result<Option<Child>> {
        self.spawn_create_after(&[])
    }

    /// [`Tree::spawn_create`], running the command `before` first under the
    /// same lock, with its output in the same log; if it fails, the hook
    /// doesn't run and its exit code is recorded as the hook's. For `wt new`,
    /// which copies inputs this way.
    #[doc(hidden)]
    pub fn spawn_create_after(&self, before: &[OsString]) -> Result<Option<Child>> {
        let Some(mut command) = self.hook_command(Hook::Create, false, before)? else {
            return Ok(None);
        };
        let lock = self.lock_lifecycle(None)?;
        // The create hook may bring services up again.
        self.set_stopped(false)?;
        self.mark_creating()?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::run::pass_locks(&mut command, &lock, None);
        // SAFETY: setsid is async-signal-safe. A session of its own keeps
        // the hook running when the terminal that started it closes.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        command.spawn().map(Some).map_err(|source| Error::Io {
            context: format!("starting {}'s create hook", self.id()),
            source,
        })
    }

    pub fn create_status(&self) -> CreateStatus {
        match fs::read_to_string(self.root.join(".wt/create.status")) {
            Err(_) => CreateStatus::NotRun,
            Ok(text) => match text.trim() {
                "running" => CreateStatus::Running,
                "0" => CreateStatus::Succeeded,
                code => CreateStatus::Failed(code.parse().unwrap_or(-1)),
            },
        }
    }

    /// Records that setting the tree up has begun, so the `[bin]` wrappers
    /// refuse from now until the `create` hook has finished, rather than
    /// run a binary copied from the main checkout. For a caller that copies
    /// inputs and runs `create` later, in the background.
    pub fn mark_creating(&self) -> Result<()> {
        if self.config()?.lifecycle.create.is_none() {
            return Ok(());
        }
        crate::fsx::write_atomic(&self.root.join(".wt/create.status"), b"running", None)
    }

    /// Where the `create` hook's output is kept.
    pub fn create_log(&self) -> PathBuf {
        self.root.join(".wt/create.log")
    }
}
