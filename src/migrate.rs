//! One-time import of trees from wt 0.5, whose registry lives in
//! `registry.json` in the same home. Trees keep their names, port slots and
//! port offsets, so running services and names derived from them don't
//! change.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Error, Result, io_at};
use crate::state::{self, TreeRecord};
use crate::{Wt, names};

#[derive(Debug, Default)]
pub struct Imported {
    /// `label/name` of each imported tree.
    pub trees: Vec<String>,
    pub warnings: Vec<String>,
}

impl Wt {
    /// Whether this home has wt 0.5's registry and nothing imported yet.
    pub fn needs_import(&self) -> bool {
        self.home.join("registry.json").is_file() && !self.home.join("state.json").exists()
    }

    /// Imports wt 0.5's repositories and trees, then clears the 0.5
    /// artifacts wt itself put in each tree (`.wt/shims`, `.wt/tree_id`,
    /// `.wt/build.status`). It doesn't render anything: each repository's
    /// config has to be converted first, and the next `wt init`, `env`,
    /// `exec` or `shell` in a tree renders it.
    pub fn import_legacy(&self) -> Result<Imported> {
        if !self.needs_import() {
            return Err(Error::Conflict(format!(
                "nothing to import: {} needs registry.json and no state.json",
                self.home.display()
            )));
        }
        let path = self.home.join("registry.json");
        let text = fs::read_to_string(&path).map_err(io_at("reading", &path))?;
        let registry: Value = serde_json::from_str(&text)
            .map_err(|error| Error::State(format!("{} is not valid: {error}", path.display())))?;
        let geometry = self.settings.ports;
        let mut imported = Imported::default();
        let mut repos = BTreeMap::new();
        if let Some(labels) = registry["labels"].as_object() {
            for (label, record) in labels {
                if let Some(main) = record["path"].as_str() {
                    repos.insert(label.clone(), PathBuf::from(main));
                }
            }
        }
        let mut trees = Vec::new();
        for record in registry["trees"].as_array().into_iter().flatten() {
            let field = |key: &str| record[key].as_str().unwrap_or_default().to_owned();
            let (label, name, root) = (field("label"), field("name"), PathBuf::from(field("path")));
            let id = format!("{label}/{name}");
            if !names::valid(&label) || !names::valid(&name) {
                imported
                    .warnings
                    .push(format!("skipped {id}: not a valid tree name any more"));
                continue;
            }
            if !root.is_dir() {
                imported
                    .warnings
                    .push(format!("skipped {id}: {} is gone", root.display()));
                continue;
            }
            let base = record["geometry"]["base"].as_u64();
            let stride = record["geometry"]["stride"].as_u64();
            if base != Some(u64::from(geometry.base)) || stride != Some(u64::from(geometry.stride))
            {
                imported.warnings.push(format!(
                    "{id}'s ports will move: it used base {base:?} and stride {stride:?}, \
                     and ~/.wt/config.toml now says base {} and stride {}",
                    geometry.base, geometry.stride
                ));
            }
            let ports = record["ports"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(port, offset)| {
                    Some((port.clone(), u16::try_from(offset.as_u64()?).ok()?))
                })
                .collect();
            trees.push(TreeRecord {
                label,
                name,
                path: root,
                slot: record["slot"].as_u64().unwrap_or_default() as u32,
                ports,
            });
            imported.trees.push(id);
        }
        state::update_and_write(&self.home, |state| {
            state.repos = repos;
            state.trees = trees.clone();
            Ok(())
        })?;
        for tree in &trees {
            clear_old_artifacts(&tree.path, &mut imported.warnings);
        }
        Ok(imported)
    }
}

/// Removes what wt 0.5 wrote into a tree for its own bookkeeping, leaving
/// everything the repository's config put there.
fn clear_old_artifacts(root: &Path, warnings: &mut Vec<String>) {
    let directory = root.join(".wt");
    let shims = directory.join("shims");
    if let Ok(entries) = fs::read_dir(&shims) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                let _ = fs::remove_file(entry.path());
            }
        }
        if fs::remove_dir(&shims).is_err() {
            warnings.push(format!(
                "left {} (it isn't only wt's symlinks)",
                shims.display()
            ));
        }
    }
    for file in ["tree_id", "build.status"] {
        let _ = fs::remove_file(directory.join(file));
    }
}
