//! Parameterised git worktrees.
//!
//! A *tree* is a git worktree wt knows about. Each gets its own block of
//! ports, an environment rendered from the repository's config, files
//! rendered or copied into it, and wrappers in `.wt/bin` for executables
//! built inside it. The CLI in `main.rs` is a thin layer over this library,
//! which other tools can embed to do the same.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let repo = std::path::PathBuf::from("/src/app");
//! use std::process::Command;
//!
//! let wt = wt::Wt::open()?;
//! let tree = wt.new_tree(&repo, wt::NewTree::named("fix-scroll"))?.tree;
//! tree.copy_inputs()?;
//! if let Some(mut create) = tree.hook(wt::Hook::Create)? {
//!     create.status()?; // or spawn it and stream its output somewhere
//! }
//! let mut agent = Command::new("claude");
//! tree.env()?.apply(&mut agent);
//! agent.current_dir(&tree.root).spawn()?;
//!
//! // Later, when the work is done. Check first, so the teardown hooks never
//! // run on a tree that then can't be removed.
//! wt.check_remove(&tree, wt::Remove::default())?;
//! for hook in [wt::Hook::Stop, wt::Hook::Delete] {
//!     if let Some(mut command) = tree.hook(hook)? {
//!         command.status()?;
//!     }
//! }
//! wt.remove(&tree, wt::Remove::default())?;
//! # Ok(())
//! # }
//! ```

pub mod config;
mod error;
mod fsx;
mod git;
mod lifecycle;
mod migrate;
mod names;
mod pool;
mod ports;
pub mod settings;
pub mod state;
mod template;
mod tree;

use std::path::{Path, PathBuf};

pub use error::{Error, Result};
pub use lifecycle::{CreateStatus, Created, Hook, NewTree, Remove, Removed};
pub use migrate::Imported;
pub use pool::{Holder, Lease, Pool, Retired, SlotInfo};
pub use settings::Settings;
pub use tree::{Env, Tree};

use config::Config;

/// A handle on wt's home directory and settings.
#[derive(Clone, Debug)]
pub struct Wt {
    home: PathBuf,
    settings: Settings,
}

/// Options for [`Wt::init`].
#[derive(Clone, Debug, Default)]
pub struct Init {
    /// The repository's label, used the first time a repository is seen.
    /// Defaults to the main checkout's directory name.
    pub label: Option<String>,
    /// The tree's name. Defaults to `main` for the main checkout and to the
    /// directory name otherwise.
    pub name: Option<String>,
}

impl Wt {
    /// Opens `$WT_HOME`, or `~/.wt`.
    pub fn open() -> Result<Wt> {
        let home = std::env::var_os("WT_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| settings::home_dir().join(".wt"));
        Self::with_home(home)
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Result<Wt> {
        let home = home.into();
        let settings = Settings::load(&home)?;
        Ok(Wt { home, settings })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Makes the worktree containing `path` a tree: registers its repository
    /// if needed, gives it a block of ports and renders its files. Calling it
    /// again on the same worktree keeps its name and ports and re-renders.
    /// Setting it up — [`Tree::copy_inputs`], then the `create` hook — is
    /// left to the caller.
    pub fn init(&self, path: &Path, options: Init) -> Result<Tree> {
        let root = git::toplevel(path)?;
        let main = git::main_checkout(&root)?;
        let label = self.label_for(&main, options.label)?;
        let config = Config::load(&self.home, &label, &root)?;
        let default_name = if root == main {
            "main".to_owned()
        } else {
            file_name(&root)
        };
        let record = state::update(&self.home, |state| {
            register(state, &label, &main)?;
            if let Some(index) = state.trees.iter().position(|tree| tree.path == root) {
                let record = &mut state.trees[index];
                if let Some(name) = &options.name
                    && *name != record.name
                {
                    return Err(Error::Conflict(format!(
                        "{} is already the tree {}/{}",
                        root.display(),
                        record.label,
                        record.name
                    )));
                }
                ports::assign(&mut record.ports, &config.ports, self.settings.ports)?;
                return Ok((record.clone(), false));
            }
            let name = options.name.clone().unwrap_or(default_name);
            if !names::valid(&name) {
                return Err(Error::Conflict(format!(
                    "`{name}` can't be a tree name; pass --name"
                )));
            }
            if let Some(other) = state.tree(&label, &name) {
                return Err(Error::Conflict(format!(
                    "{label}/{name} already exists at {}; pass --name",
                    other.path.display()
                )));
            }
            let slot = ports::allocate_slot(state, self.settings.ports)?;
            let mut record = state::TreeRecord {
                label: label.clone(),
                name,
                path: root.clone(),
                slot,
                ports: Default::default(),
            };
            ports::assign(&mut record.ports, &config.ports, self.settings.ports)?;
            state.trees.push(record.clone());
            Ok((record, true))
        })?;
        let (record, created) = record;
        let tree = Tree::from_record(self, &record, main);
        if let Err(error) = tree.env() {
            // A tree that never got going shouldn't hold on to its name and
            // ports.
            if created {
                state::update(&self.home, |state| {
                    state.trees.retain(|other| other.path != record.path);
                    Ok(())
                })?;
            }
            return Err(error);
        }
        Ok(tree)
    }

    /// Every tree, in the order they were made.
    pub fn list(&self) -> Result<Vec<Tree>> {
        let state = state::read(&self.home)?;
        Ok(state
            .trees
            .iter()
            .map(|record| self.tree_from(&state, record))
            .collect())
    }

    /// Resolves `spec` — `label/name`, or a `name` that is unambiguous — or,
    /// without one, the tree containing `cwd`.
    pub fn find(&self, spec: Option<&str>, cwd: &Path) -> Result<Tree> {
        let state = state::read(&self.home)?;
        let containing = || {
            let cwd = std::fs::canonicalize(cwd).ok()?;
            state
                .trees
                .iter()
                .filter(|tree| cwd.starts_with(&tree.path))
                .max_by_key(|tree| tree.path.components().count())
        };
        let record = match spec {
            None => containing().ok_or_else(|| {
                Error::NotFound(format!(
                    "{} is not inside a tree; name one, or run `wt init` here",
                    cwd.display()
                ))
            })?,
            Some(spec) => match spec.split_once('/') {
                Some((label, name)) => state
                    .tree(label, name)
                    .ok_or_else(|| Error::NotFound(format!("no tree named {spec}")))?,
                None => {
                    let matches: Vec<_> = state
                        .trees
                        .iter()
                        .filter(|tree| tree.name == spec)
                        .collect();
                    let here = containing().map(|tree| tree.label.clone());
                    match matches.as_slice() {
                        [] => return Err(Error::NotFound(format!("no tree named {spec}"))),
                        [only] => only,
                        several => several
                            .iter()
                            .find(|tree| Some(&tree.label) == here.as_ref())
                            .copied()
                            .ok_or_else(|| {
                                let ids: Vec<_> = several
                                    .iter()
                                    .map(|tree| format!("{}/{}", tree.label, tree.name))
                                    .collect();
                                Error::NotFound(format!(
                                    "`{spec}` is ambiguous: {}",
                                    ids.join(", ")
                                ))
                            })?,
                    }
                }
            },
        };
        Ok(self.tree_from(&state, record))
    }

    /// The label of the repository whose main checkout is `main`: the one it
    /// is registered under, or else `asked`, or else the directory's name.
    fn label_for(&self, main: &Path, asked: Option<String>) -> Result<String> {
        let state = state::read(&self.home)?;
        let label = match (state.label_for(main), asked) {
            (Some(existing), Some(asked)) if existing != asked => {
                return Err(Error::Conflict(format!(
                    "{} is already registered as `{existing}`",
                    main.display()
                )));
            }
            (Some(existing), _) => existing.to_owned(),
            (None, Some(asked)) => asked,
            (None, None) => file_name(main),
        };
        if !names::valid(&label) {
            return Err(Error::Conflict(format!(
                "`{label}` can't be a label; pass --label"
            )));
        }
        Ok(label)
    }

    fn tree_from(&self, state: &state::State, record: &state::TreeRecord) -> Tree {
        let main = state
            .repos
            .get(&record.label)
            .cloned()
            .unwrap_or_else(|| record.path.clone());
        Tree::from_record(self, record, main)
    }
}

/// Records `label` as the repository whose main checkout is `main`, refusing
/// a label already used by another repository.
fn register(state: &mut state::State, label: &str, main: &Path) -> Result<()> {
    match state.repos.get(label) {
        Some(other) if other != main => Err(Error::Conflict(format!(
            "the label `{label}` is already used by {}; pass --label",
            other.display()
        ))),
        Some(_) => Ok(()),
        None => {
            state.repos.insert(label.to_owned(), main.to_owned());
            Ok(())
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
