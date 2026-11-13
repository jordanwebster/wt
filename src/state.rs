//! `~/.wt/state.json`: the repositories wt knows and the trees it manages.
//!
//! This file is the contract between every copy of wt on a machine — the
//! CLI and the library embedded in other tools — so it stays small and
//! carries a version. Changes go through [`update`], which holds an
//! exclusive lock on `state.lock` for the read-modify-write.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_at};
use crate::fsx;

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// Label → the repository's main checkout.
    pub repos: BTreeMap<String, PathBuf>,
    pub trees: Vec<TreeRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TreeRecord {
    pub label: String,
    pub name: String,
    pub path: PathBuf,
    /// Which block of ports the tree owns.
    pub slot: u32,
    /// Port name → offset within the block. Append-only: an offset is never
    /// reused for another name, so a port never moves while the tree lives.
    pub ports: BTreeMap<String, u16>,
}

impl State {
    pub fn tree(&self, label: &str, name: &str) -> Option<&TreeRecord> {
        self.trees
            .iter()
            .find(|tree| tree.label == label && tree.name == name)
    }

    pub fn tree_at(&self, path: &Path) -> Option<&TreeRecord> {
        self.trees.iter().find(|tree| tree.path == path)
    }

    pub fn label_for(&self, main: &Path) -> Option<&str> {
        self.repos
            .iter()
            .find(|(_, path)| path.as_path() == main)
            .map(|(label, _)| label.as_str())
    }
}

/// Reads the state without locking. Writes replace the file atomically, so
/// this always sees a whole state.
pub fn read(home: &Path) -> Result<State> {
    let path = home.join("state.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(State {
                version: VERSION,
                ..State::default()
            });
        }
        Err(error) => return Err(io_at("reading", &path)(error)),
    };
    let state: State = serde_json::from_str(&text)
        .map_err(|error| Error::State(format!("{} is not valid: {error}", path.display())))?;
    if state.version > VERSION {
        return Err(Error::State(format!(
            "{} was written by a newer wt (state version {}); upgrade wt",
            path.display(),
            state.version
        )));
    }
    Ok(state)
}

/// Runs `change` on the current state under the state lock and writes the
/// result if it changed.
pub fn update<T>(home: &Path, change: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
    locked(home, false, change)
}

/// Like [`update`], but always writes, so the state file exists afterwards.
pub fn update_and_write<T>(home: &Path, change: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
    locked(home, true, change)
}

fn locked<T>(
    home: &Path,
    always_write: bool,
    change: impl FnOnce(&mut State) -> Result<T>,
) -> Result<T> {
    if !always_write && home.join("registry.json").is_file() && !home.join("state.json").exists() {
        return Err(Error::State(format!(
            "{} has trees from wt 0.5; run `wt migrate` to import them first",
            home.display()
        )));
    }
    fs::create_dir_all(home).map_err(io_at("creating", home))?;
    let lock_path = home.join("state.lock");
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(io_at("opening", &lock_path))?;
    lock.lock().map_err(io_at("locking", &lock_path))?;
    let mut state = read(home)?;
    let before = state.clone();
    let value = change(&mut state)?;
    if always_write || state != before {
        state.version = VERSION;
        let mut text = serde_json::to_string_pretty(&state).expect("state serialises");
        text.push('\n');
        fsx::write_atomic(&home.join("state.json"), text.as_bytes(), None)?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_newer_versions() {
        let home = tempfile::tempdir().unwrap();
        update(home.path(), |state| {
            state.repos.insert("r".into(), "/r".into());
            Ok(())
        })
        .unwrap();
        assert_eq!(read(home.path()).unwrap().repos["r"], Path::new("/r"));

        fs::write(
            home.path().join("state.json"),
            r#"{"version": 99, "repos": {}, "trees": []}"#,
        )
        .unwrap();
        let error = read(home.path()).unwrap_err().to_string();
        assert!(error.contains("upgrade wt"), "{error}");
    }
}
