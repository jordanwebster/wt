//! A tree and the environment it renders.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Wt;
use crate::config::Config;
use crate::error::{Result, io_at};
use crate::settings::expand_home;
use crate::state::{self, TreeRecord};
use crate::{fsx, git, names, ports, template};

/// A worktree wt manages.
#[derive(Clone, Debug)]
pub struct Tree {
    pub(crate) wt: Wt,
    pub label: String,
    pub name: String,
    pub root: PathBuf,
    /// The repository's main checkout. Equal to `root` for the main checkout
    /// itself.
    pub main: PathBuf,
    pub slot: u32,
    pub(crate) offsets: BTreeMap<String, u16>,
}

/// What a process needs to run inside a tree: variables to set, and
/// directories to put in front of `PATH`.
#[derive(Clone, Debug, PartialEq)]
pub struct Env {
    pub vars: BTreeMap<String, String>,
    pub path_prefix: Vec<PathBuf>,
}

impl Env {
    /// `PATH` for the tree, given the `PATH` it is entered from.
    pub fn path(&self, inherited: Option<&OsStr>) -> OsString {
        // Joined by hand rather than with `join_paths`, which refuses a
        // directory containing `:`. Such a directory can't be found through
        // PATH, but entering the tree shouldn't fail because of it.
        let mut path = OsString::new();
        for directory in &self.path_prefix {
            path.push(directory);
            path.push(":");
        }
        match inherited {
            Some(inherited) => path.push(inherited),
            None => {
                path = path.to_string_lossy().trim_end_matches(':').into();
            }
        }
        path
    }

    /// Sets the tree's variables and `PATH` (relative to this process's
    /// `PATH`) on `command`.
    pub fn apply(&self, command: &mut Command) {
        command.envs(&self.vars);
        command.env("PATH", self.path(std::env::var_os("PATH").as_deref()));
    }

    /// `export` lines for `sh`, for `eval "$(wt env)"`.
    pub fn to_shell(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.vars {
            out.push_str(&format!("export {key}={}\n", fsx::sh_quote(value)));
        }
        if !self.path_prefix.is_empty() {
            let prefix = self.path(None);
            out.push_str(&format!(
                "export PATH={}:\"$PATH\"\n",
                fsx::sh_quote(&prefix.to_string_lossy())
            ));
        }
        out
    }
}

/// Second line of every generated `[bin]` wrapper, so stale ones can be
/// told apart from files the repository's config put in `.wt/bin`.
const WRAPPER_MARK: &str = "# wt:bin";

/// Brackets wt's block in the repository's shared `info/exclude`.
const EXCLUDE_BEGIN: &str = "# >>> wt: files wt writes into trees";
const EXCLUDE_END: &str = "# <<< wt";

impl Tree {
    pub(crate) fn from_record(wt: &Wt, record: &TreeRecord, main: PathBuf) -> Tree {
        Tree {
            wt: wt.clone(),
            label: record.label.clone(),
            name: record.name.clone(),
            root: record.path.clone(),
            main,
            slot: record.slot,
            offsets: record.ports.clone(),
        }
    }

    /// `label/name`.
    pub fn id(&self) -> String {
        format!("{}/{}", self.label, self.name)
    }

    /// The checked-out branch, or `None` when HEAD is detached or the
    /// worktree is gone.
    pub fn branch(&self) -> Option<String> {
        git::current_branch(&self.root)
    }

    pub fn is_main(&self) -> bool {
        self.root == self.main
    }

    /// Port name → port number, for the ports assigned so far.
    pub fn ports(&self) -> BTreeMap<String, u16> {
        self.offsets
            .iter()
            .map(|(name, offset)| {
                let port = ports::port(self.wt.settings.ports, self.slot, *offset);
                (name.clone(), port)
            })
            .collect()
    }

    pub fn config(&self) -> Result<Config> {
        Config::load(&self.wt.home, &self.label, &self.root)
    }

    /// Brings the tree's rendered files, `[bin]` wrappers and ports up to
    /// date with its config, and returns its environment. Cheap when nothing
    /// changed: files are rewritten only when their content differs.
    pub fn env(&self) -> Result<Env> {
        if !self.root.is_dir() {
            return Err(crate::Error::NotFound(format!(
                "{}'s directory {} is gone; `wt rm --keep {}` forgets it",
                self.id(),
                self.root.display(),
                self.id()
            )));
        }
        let config = self.config()?;
        let tree = self.with_ports(&config)?;
        let mut vars = tree.template_vars();
        let mut env = tree.identity_vars();
        for (key, value) in &config.env {
            let rendered = template::render(value, &vars, &format!("env.{key}"))?;
            env.insert(key.clone(), rendered);
        }
        for (key, value) in &env {
            vars.insert(format!("env.{key}"), value.clone());
        }
        for (path, file) in &config.files {
            let content = template::render(&file.content, &vars, &format!("files.\"{path}\""))?;
            fsx::write_if_changed(&tree.root.join(path), content.as_bytes(), file.mode)?;
        }
        tree.write_wrappers(&config)?;
        tree.update_excludes(&config)?;
        Ok(Env {
            vars: env,
            path_prefix: vec![tree.root.join(".wt/bin")],
        })
    }

    /// Copies the config's `copy` entries that aren't in the tree yet. Part
    /// of setting a tree up, with the `create` hook, and like it left to
    /// the caller because copying a large build directory takes seconds.
    pub fn copy_inputs(&self) -> Result<()> {
        let config = self.config()?;
        for entry in &config.copy {
            let source = if entry.from.starts_with("~/") || Path::new(&entry.from).is_absolute() {
                expand_home(&entry.from)
            } else {
                self.main.join(&entry.from)
            };
            let destination = self.root.join(&entry.to);
            if source == destination
                || fs::symlink_metadata(&destination).is_ok()
                || fs::symlink_metadata(&source).is_err()
            {
                continue;
            }
            fsx::copy(&source, &destination)?;
        }
        Ok(())
    }

    /// This tree with an offset for every port its config declares,
    /// recording any new ones.
    fn with_ports(&self, config: &Config) -> Result<Tree> {
        if config
            .ports
            .iter()
            .all(|name| self.offsets.contains_key(name))
        {
            return Ok(self.clone());
        }
        let geometry = self.wt.settings.ports;
        let offsets = state::update(&self.wt.home, |state| {
            let record = state
                .trees
                .iter_mut()
                .find(|tree| tree.path == self.root)
                .ok_or_else(|| crate::Error::NotFound(format!("{} was removed", self.id())))?;
            ports::assign(&mut record.ports, &config.ports, geometry)?;
            Ok(record.ports.clone())
        })?;
        Ok(Tree {
            offsets,
            ..self.clone()
        })
    }

    fn template_vars(&self) -> BTreeMap<String, String> {
        let mut vars = BTreeMap::from([
            ("name".to_owned(), self.name.clone()),
            ("name_snake".to_owned(), names::snake(&self.name)),
            (
                "name_short".to_owned(),
                names::short(&self.label, &self.name),
            ),
            ("label".to_owned(), self.label.clone()),
            ("root".to_owned(), self.root.display().to_string()),
            ("main".to_owned(), self.main.display().to_string()),
        ]);
        for (name, port) in self.ports() {
            vars.insert(format!("ports.{name}"), port.to_string());
        }
        vars
    }

    fn identity_vars(&self) -> BTreeMap<String, String> {
        let mut vars = BTreeMap::from([
            ("WT_TREE".to_owned(), self.id()),
            ("WT_ROOT".to_owned(), self.root.display().to_string()),
            ("WT_NAME".to_owned(), self.name.clone()),
            ("WT_LABEL".to_owned(), self.label.clone()),
            ("WT_MAIN".to_owned(), self.main.display().to_string()),
            ("WT_HOME".to_owned(), self.wt.home.display().to_string()),
            // For shell startup files that reorder PATH to put it back first.
            (
                "WT_BIN".to_owned(),
                self.root.join(".wt/bin").display().to_string(),
            ),
        ]);
        for (name, port) in self.ports() {
            vars.insert(
                format!("WT_PORT_{}", names::env_suffix(&name)),
                port.to_string(),
            );
        }
        vars
    }

    /// Writes a wrapper in `.wt/bin` for each `[bin]` entry, and removes
    /// wrappers for entries that are gone.
    fn write_wrappers(&self, config: &Config) -> Result<()> {
        let bin = self.root.join(".wt/bin");
        if let Ok(entries) = fs::read_dir(&bin) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let generated = fs::read_to_string(entry.path())
                    .is_ok_and(|text| text.lines().nth(1) == Some(WRAPPER_MARK));
                if generated && !config.bin.contains_key(&name) {
                    fs::remove_file(entry.path()).map_err(io_at("removing", &entry.path()))?;
                }
            }
        }
        for (name, relative) in &config.bin {
            let script = self.wrapper(name, relative);
            fsx::write_if_changed(&bin.join(name), script.as_bytes(), Some(0o755))?;
        }
        Ok(())
    }

    /// A script that runs this tree's build of `name`, and otherwise says
    /// why it can't rather than letting the shell find an installed copy.
    fn wrapper(&self, name: &str, relative: &str) -> String {
        let executable = fsx::sh_quote(&self.root.join(relative).display().to_string());
        let status = fsx::sh_quote(&self.root.join(".wt/create.status").display().to_string());
        let log = fsx::sh_quote(&self.root.join(".wt/create.log").display().to_string());
        let id = self.id();
        format!(
            r#"#!/bin/sh
{WRAPPER_MARK}
# Runs {name} as built in {id}. Generated from [bin] in the tree's config.
# While the create hook runs, or after it failed, a binary already there is
# not this tree's build: it may have been copied from the main checkout.
exe={executable}
log={log}
case "$(cat {status} 2>/dev/null)" in
  running) why="the create hook is still running (or was interrupted); see $log" ;;
  ''|0) [ -x "$exe" ] && exec "$exe" "$@"; why="$exe is missing; build it in this tree" ;;
  *) why="the create hook failed; see $log" ;;
esac
echo "{name}: not ready in {id}: $why" >&2
exit 127
"#
        )
    }

    /// Adds the paths wt writes into trees to the repository's shared
    /// `info/exclude`, inside wt's own block. Only ever adds: several
    /// branches may declare different files, and listing a path that no
    /// longer exists is harmless.
    fn update_excludes(&self, config: &Config) -> Result<()> {
        let mut wanted: BTreeSet<String> = BTreeSet::from(["/.wt/".to_owned()]);
        wanted.extend(config.files.keys().map(|path| format!("/{path}")));
        wanted.extend(config.copy.iter().map(|entry| format!("/{}", entry.to)));
        let path = git::common_dir(&self.root)?.join("info/exclude");
        let listed = |text: &str| -> BTreeSet<String> {
            split_block(text).1.lines().map(str::to_owned).collect()
        };
        if wanted.is_subset(&listed(&fs::read_to_string(&path).unwrap_or_default())) {
            return Ok(());
        }
        // Trees of one repository share the file, so two entering at once
        // would each drop the other's lines without the lock.
        let lock_path = self.wt.home.join("exclude.lock");
        fs::create_dir_all(&self.wt.home).map_err(io_at("creating", &self.wt.home))?;
        let lock = fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(io_at("opening", &lock_path))?;
        lock.lock().map_err(io_at("locking", &lock_path))?;
        let text = fs::read_to_string(&path).unwrap_or_default();
        let (before, _, after) = split_block(&text);
        let existing = listed(&text);
        let mut lines: Vec<String> = existing.union(&wanted).cloned().collect();
        lines.retain(|line| !line.is_empty());
        let mut out = before.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(EXCLUDE_BEGIN);
        out.push('\n');
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
        out.push_str(EXCLUDE_END);
        out.push('\n');
        out.push_str(after);
        fsx::write_atomic(&path, out.as_bytes(), None)
    }
}

/// Splits `text` into what precedes wt's exclude block, the block's lines,
/// and what follows it.
fn split_block(text: &str) -> (&str, &str, &str) {
    let Some(start) = text.find(EXCLUDE_BEGIN) else {
        return (text, "", "");
    };
    let body_start = start + EXCLUDE_BEGIN.len();
    let Some(end) = text[body_start..].find(EXCLUDE_END) else {
        return (text, "", "");
    };
    let body = text[body_start..body_start + end].trim_matches('\n');
    let rest = &text[body_start + end + EXCLUDE_END.len()..];
    (
        &text[..start],
        body,
        rest.strip_prefix('\n').unwrap_or(rest),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_the_exclude_block() {
        let text = format!("a\n{EXCLUDE_BEGIN}\n/x\n/y\n{EXCLUDE_END}\nb\n");
        assert_eq!(split_block(&text), ("a\n", "/x\n/y", "b\n"));
        assert_eq!(split_block("a\n"), ("a\n", "", ""));
    }
}
