use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wt_core::resource::{ResourceHold, ResourceKey};
use wt_core::{CoreError, ExitClass};

use crate::cli::Leases;

use super::{door, executor, with_resources, Context, Output};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Keep {
    pub tree: String,
    pub until_millis: u64,
    pub resource: ResourceKey,
}

#[derive(Serialize)]
struct LeaseRow {
    pool: String,
    slot: u16,
    name: String,
    present: Option<bool>,
    hold: String,
    holder: Option<serde_json::Value>,
    released_at: Option<String>,
}

pub(crate) fn run(context: &mut Context, args: Leases) -> Result<Output, CoreError> {
    let door = door::enter(context, args.label.as_deref(), "leases")?;
    let scope = wt_core::config::effective_scope(&door.config, ".")?;
    let mut rows = Vec::new();
    for (name, pool) in scope
        .resources
        .iter()
        .filter(|(_, pool)| pool.count.is_some())
    {
        let count = pool.count.unwrap_or(1);
        let dir = with_resources::lease_dir(context, &door, name, pool.tied_to);
        let occupancy = wt_sys::lock::named_occupancy(&dir, count)?;
        for slot in 0..count {
            let process = occupancy.holders.iter().find(|entry| entry.slot == slot);
            let keep = active_keep(&keep_path(&dir, slot))?;
            let lifecycle = if pool.token_pool() {
                None
            } else {
                let id = format!("{name}#{}", slot + 1);
                let plan = executor::plan(context, &door, &id)?;
                plan.nodes
                    .iter()
                    .find(|node| node.id == id)
                    .and_then(|node| node.resource.as_ref())
                    .and_then(|key| executor::load_record(context, &door, key).ok())
            };
            let (hold, holder) = if let Some(process) = process {
                (
                    "leased".to_owned(),
                    process
                        .holder
                        .as_ref()
                        .and_then(|holder| serde_json::to_value(holder).ok()),
                )
            } else if let Some(keep) = keep {
                (
                    "kept".to_owned(),
                    Some(serde_json::json!({"tree": keep.tree, "until_millis": keep.until_millis})),
                )
            } else {
                ("free".to_owned(), None)
            };
            if hold == "free"
                && lifecycle
                    .as_ref()
                    .is_some_and(|record| record.hold != ResourceHold::Free)
            {
                let key = lifecycle
                    .as_ref()
                    .expect("checked lifecycle record")
                    .key
                    .clone();
                executor::set_instance_hold(context, &door, &key, ResourceHold::Free)?;
            }
            rows.push(LeaseRow {
                pool: name.clone(),
                slot: slot + 1,
                name: pool.name.as_ref().map_or_else(
                    || format!("{name}-{}", slot + 1),
                    |template| template.replace("{{index()}}", &(slot + 1).to_string()),
                ),
                present: lifecycle.as_ref().map(|record| record.instance.is_some()),
                hold,
                holder,
                released_at: lifecycle.and_then(|record| record.released_at),
            });
        }
    }
    Output::data(serde_json::json!({"leases": rows}))
}

pub(crate) fn keep_path(dir: &Path, slot: u16) -> PathBuf {
    dir.join(format!("{slot}.keep.json"))
}

pub(crate) fn active_keep(path: &Path) -> Result<Option<Keep>, CoreError> {
    let keep = wt_sys::fsx::read_json::<Keep>(path, "LEASE_STATE_CORRUPT")?;
    if keep
        .as_ref()
        .is_some_and(|keep| keep.until_millis <= now_millis())
    {
        wt_sys::fsx::remove_path(path)?;
        return Ok(None);
    }
    Ok(keep)
}

pub(crate) fn write_keep(path: &Path, keep: &Keep) -> Result<(), CoreError> {
    wt_sys::fsx::write_json(path, keep)
}

pub(crate) fn clear_keep(path: &Path) -> Result<(), CoreError> {
    wt_sys::fsx::remove_path(path).map(|_| ())
}

pub(crate) fn release_tree(context: &Context, target: &str) -> Result<(), CoreError> {
    for path in keep_files(&context.home.join("leases"))? {
        if let Some(keep) = wt_sys::fsx::read_json::<Keep>(&path, "LEASE_STATE_CORRUPT")?
            .filter(|keep| keep.tree == target)
        {
            if let Ok(target) = wt_core::model::Target::parse(target) {
                executor::clear_stored_keep(context, &target, &keep.resource)?;
            }
            wt_sys::fsx::remove_path(&path)?;
        }
    }
    Ok(())
}

fn keep_files(root: &Path) -> Result<Vec<PathBuf>, CoreError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(error)),
        };
        for entry in entries {
            let path = entry.map_err(io_error)?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".keep.json"))
            {
                files.push(path);
            }
        }
    }
    Ok(files)
}

pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn io_error(error: std::io::Error) -> CoreError {
    CoreError::new(
        ExitClass::Internal,
        "IO_FAILED",
        error.to_string(),
        "retry and inspect filesystem permissions",
    )
}
