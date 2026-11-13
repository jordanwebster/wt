//! Creating and removing trees, and the hooks that run around them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::config::Config;
use crate::error::{Error, Result, io_at};
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

/// A request to make a tree with `git worktree add`.
#[derive(Clone, Debug)]
pub struct NewTree {
    /// The repository's label. Defaults to the repository containing the
    /// directory `new_tree` is given.
    pub label: Option<String>,
    pub name: String,
    /// What to start from instead of `origin/<default branch>`.
    pub from: Option<String>,
    /// The branch, instead of one rendered from the config's `branch`
    /// templates (or the tree's name when there are none).
    pub branch: Option<String>,
    /// Values for the `branch` templates.
    pub vars: BTreeMap<String, String>,
    /// Fetch the default branch first.
    pub fetch: bool,
}

impl NewTree {
    pub fn named(name: impl Into<String>) -> NewTree {
        NewTree {
            label: None,
            name: name.into(),
            from: None,
            branch: None,
            vars: BTreeMap::new(),
            fetch: true,
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
    pub force: bool,
    /// Leave the worktree on disk and only stop managing it.
    pub keep: bool,
}

#[derive(Debug)]
pub struct Removed {
    /// The branch the tree had checked out, which is kept.
    pub branch: Option<String>,
}

/// A fetch that hangs shouldn't hold up making a tree: the last fetched copy
/// of the default branch is a fine place to start.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs the `create` hook, recording `running` and then its exit code in
/// `.wt/create.status`, and copying its output to `.wt/create.log`.
const CREATE_WRAPPER: &str = r#"status="$1"; log="$2"; hook="$3"
printf running > "$status"
{ sh -c "$hook" 2>&1; echo $? > "$status.exit"; } | tee "$log"
code=$(cat "$status.exit"); rm -f "$status.exit"
printf '%s' "$code" > "$status"
exit "$code""#;

impl Wt {
    /// Makes a new worktree under the trees directory and initialises it.
    /// It starts from `origin/<default branch>` (after a fetch that falls
    /// back to the last fetched copy) unless told otherwise; the main
    /// checkout and its branches are never touched. Doesn't run the
    /// `create` hook: see [`Tree::hook`].
    pub fn new_tree(&self, cwd: &Path, request: NewTree) -> Result<Created> {
        let (label, main) = self.repository(request.label.as_deref(), cwd)?;
        let name = request.name.clone();
        if !names::valid(&name) {
            return Err(Error::Conflict(format!("`{name}` can't be a tree name")));
        }
        if let Some(other) = state::read(&self.home)?.tree(&label, &name) {
            return Err(Error::Conflict(format!(
                "{label}/{name} already exists at {}",
                other.path.display()
            )));
        }
        let path = self.settings.trees_dir.join(&label).join(&name);
        if path.exists() {
            return Err(Error::Conflict(format!(
                "{} already exists",
                path.display()
            )));
        }
        let config = Config::load(&self.home, &label, &main)?;
        let branch = branch_for(&request, &config, &label)?;
        git::run(&main, &["check-ref-format", "--branch", &branch])
            .map_err(|_| Error::Conflict(format!("`{branch}` isn't a valid branch name")))?;

        let mut warnings = Vec::new();
        let local = git::ref_exists(&main, &format!("refs/heads/{branch}"));
        let remote_ref = format!("refs/remotes/origin/{branch}");
        // A branch asked for by name may be someone else's on origin: fetch
        // it, so that it is checked out rather than started afresh. It may
        // equally be new, so a failed fetch says nothing.
        if !local && request.branch.is_some() && request.fetch && git::has_origin(&main) {
            let _ = git::run_timeout(
                &main,
                &[
                    "fetch",
                    "--quiet",
                    "origin",
                    &format!("+refs/heads/{branch}:{remote_ref}"),
                ],
                FETCH_TIMEOUT,
            );
        }
        let remote = !local && git::ref_exists(&main, &remote_ref);
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
            None => start_point(&main, request.fetch, &mut warnings),
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
        match self.init(&path, init) {
            Ok(tree) => Ok(Created { tree, warnings }),
            Err(error) => {
                let _ = git::run(&main, &["worktree", "remove", "--force", &path_arg]);
                if !local {
                    let _ = git::run(&main, &["branch", "-D", &branch]);
                }
                Err(error)
            }
        }
    }

    /// Refuses a removal [`Wt::remove`] would refuse. Call it before running
    /// the tree's `stop` and `delete` hooks, so they never run on a tree
    /// that then can't be removed.
    pub fn check_remove(&self, tree: &Tree, options: Remove) -> Result<()> {
        if options.keep {
            return Ok(());
        }
        if tree.is_main() {
            return Err(Error::Conflict(format!(
                "{} is the main checkout; `--keep` stops managing it without deleting it",
                tree.id()
            )));
        }
        if !options.force && tree.root.is_dir() && git::is_dirty(&tree.root)? {
            return Err(Error::Conflict(format!(
                "{} has uncommitted changes or untracked files; commit them, or pass --force",
                tree.id()
            )));
        }
        Ok(())
    }

    /// Stops managing a tree and, unless `keep`, removes its worktree. The
    /// branch is kept. Call [`Wt::check_remove`], then run the tree's `stop`
    /// and `delete` hooks, then this.
    pub fn remove(&self, tree: &Tree, options: Remove) -> Result<Removed> {
        self.check_remove(tree, options)?;
        let branch = tree.branch();
        if !options.keep {
            if tree.root.is_dir() {
                // What is left is either clean or ignored (build output, and
                // the files wt itself rendered), so forcing is safe here.
                let path = tree.root.to_string_lossy();
                git::run(&tree.main, &["worktree", "remove", "--force", &path])?;
            } else {
                let _ = git::run(&tree.main, &["worktree", "prune"]);
            }
        }
        state::update(&self.home, |state| {
            state.trees.retain(|record| record.path != tree.root);
            Ok(())
        })?;
        Ok(Removed { branch })
    }

    /// The label and main checkout of the repository named `label`, or of
    /// the one containing `cwd`, registering it if it is new.
    fn repository(&self, label: Option<&str>, cwd: &Path) -> Result<(String, PathBuf)> {
        if let Some(label) = label {
            let state = state::read(&self.home)?;
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
        state::update(&self.home, |state| crate::register(state, &label, &main))?;
        Ok((label, main))
    }
}

/// `origin/<default>` after fetching it, or the main checkout's HEAD for a
/// repository without a usable remote.
fn start_point(main: &Path, fetch: bool, warnings: &mut Vec<String>) -> String {
    if !git::has_origin(main) {
        return "HEAD".to_owned();
    }
    let Some(default) = git::default_branch(main) else {
        warnings.push(
            "couldn't tell origin's default branch; starting from the main checkout's HEAD"
                .to_owned(),
        );
        return "HEAD".to_owned();
    };
    if fetch
        && let Err(error) = git::run_timeout(
            main,
            &["fetch", "--quiet", "origin", &default],
            FETCH_TIMEOUT,
        )
    {
        warnings.push(format!(
            "couldn't fetch origin/{default} ({error}); starting from the copy fetched last"
        ));
    }
    format!("origin/{default}")
}

/// The branch for a new tree: as asked, or the first `branch` template whose
/// variables are all set, or the tree's name.
fn branch_for(request: &NewTree, config: &Config, label: &str) -> Result<String> {
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
    /// its environment. `None` when the config doesn't declare it. `create`
    /// also records its progress for [`Tree::create_status`] and the
    /// `[bin]` wrappers, and merges stderr into stdout.
    pub fn hook(&self, hook: Hook) -> Result<Option<Command>> {
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
                .arg(script);
        } else {
            command.arg("-c").arg(script);
        }
        env.apply(&mut command);
        command.current_dir(&self.root);
        Ok(Some(command))
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

    /// Where the `create` hook's output is kept.
    pub fn create_log(&self) -> PathBuf {
        self.root.join(".wt/create.log")
    }
}
