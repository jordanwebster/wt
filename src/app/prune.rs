use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use wt_core::lifecycle::{DerivedPhase, RepoState};
use wt_core::report::{Notice, NoticeLevel, PruneData, PruneItemReport};
use wt_core::resource::ResourceState;
use wt_core::{CoreError, ExitClass};
use wt_sys::lock;

use crate::cli::{Prune, Remove};

use super::{door, executor, remove, Context, Output};

pub(crate) fn run(context: &mut Context, args: Prune) -> Result<Output, CoreError> {
    let mut items = plan(context, &args)?;
    let should_apply = if context.yes {
        true
    } else if context.tty.stdin {
        context.confirm(&format!(
            "apply prune plan: {}",
            serde_json::to_string(&items).unwrap_or_else(|_| "<unavailable>".to_owned())
        ))?
    } else {
        false
    };
    if !should_apply {
        let mut output = Output::data(PruneData {
            applied: false,
            items,
        })?;
        output.notices.push(Notice {
            level: NoticeLevel::Warn,
            code: "CONFIRM_REQUIRED".to_owned(),
            subject: None,
            message: "prune plan was reported but not applied; re-run with --yes".to_owned(),
        });
        return Ok(output);
    }

    let original_yes = context.yes;
    context.yes = true;
    let result = apply(context, &args, &mut items);
    context.yes = original_yes;
    let notices = result?;
    Ok(Output::data(PruneData {
        applied: true,
        items,
    })?
    .with_notices(notices))
}

fn plan(context: &Context, args: &Prune) -> Result<Vec<PruneItemReport>, CoreError> {
    if let Some(target) = &args.records {
        let target = context.resolve(Some(target))?;
        let tree = context.tree(&target)?;
        let state = context.read_state(&target)?;
        let phase = context.phase(&tree, state.as_ref())?;
        if !matches!(
            phase,
            DerivedPhase::Missing | DerivedPhase::Replaced | DerivedPhase::RemoveInterrupted
        ) {
            return Err(CoreError::new(
                ExitClass::State,
                "PRUNE_RECORDS_REFUSED",
                format!("{target} is in phase {}", super::list::phase_name(phase)),
                "use --records only for missing, replaced, or remove-interrupted trees",
            ));
        }
        return Ok(vec![PruneItemReport {
            target: target.to_string(),
            reasons: vec!["records".to_owned()],
            action: "destroy-records".to_owned(),
            result: None,
        }]);
    }

    let mut planned = BTreeMap::<String, PruneItemReport>::new();
    for tombstone in context.registry.tombstones.iter().filter(|tombstone| {
        args.label
            .as_deref()
            .is_none_or(|label| tombstone.label.as_str() == label)
    }) {
        let target = wt_core::model::Target {
            label: tombstone.label.clone(),
            name: tombstone.name.clone(),
        }
        .to_string();
        planned.insert(
            target.clone(),
            PruneItemReport {
                target,
                reasons: vec!["tombstone".to_owned()],
                action: "collect".to_owned(),
                result: None,
            },
        );
    }
    for tree in &context.registry.trees {
        if tree.canonical
            || args
                .label
                .as_deref()
                .is_some_and(|label| tree.label.as_str() != label)
        {
            continue;
        }
        let target = super::context::target_of(tree);
        let state = context.read_state(&target)?;
        let phase = context.phase(tree, state.as_ref())?;
        let mut reasons = Vec::new();
        let mut action = "keep";
        if phase == DerivedPhase::Missing {
            reasons.push("missing".to_owned());
            action = "remove";
        } else if state.as_ref().is_some_and(|state| {
            state
                .resources
                .values()
                .any(|record| record.state == ResourceState::Orphaned)
        }) {
            reasons.push("orphaned-records".to_owned());
            action = "destroy-records";
        }
        if (args.merged || args.gone) && phase == DerivedPhase::Ready {
            if let Some(observation) = remove::observe_git(context, tree, true)? {
                let dirty = !observation.dirty_porcelain.is_empty();
                let gone = observation.upstream == wt_core::remove::Upstream::Gone;
                if args.merged && observation.merged {
                    reasons.push("merged".to_owned());
                }
                if args.gone && gone {
                    reasons.push("gone".to_owned());
                }
                if !reasons.is_empty() {
                    action = if dirty { "keep" } else { "remove" };
                    if dirty {
                        reasons.push("dirty".to_owned());
                    }
                }
            }
        }
        if !reasons.is_empty() {
            planned.insert(
                target.to_string(),
                PruneItemReport {
                    target: target.to_string(),
                    reasons,
                    action: action.to_owned(),
                    result: None,
                },
            );
        }
    }
    for path in state_orphans(context, args.label.as_deref())? {
        let target = path.to_string_lossy().into_owned();
        planned.insert(
            target.clone(),
            PruneItemReport {
                target,
                reasons: vec!["state-orphan".to_owned()],
                action: "delete-state".to_owned(),
                result: None,
            },
        );
    }
    for path in cache_orphans(context, args.label.as_deref())? {
        let target = path.to_string_lossy().into_owned();
        planned.insert(
            target.clone(),
            PruneItemReport {
                target,
                reasons: vec!["cache-orphan".to_owned()],
                action: "delete-cache".to_owned(),
                result: None,
            },
        );
    }
    for entry in stale_exclusive_entries(context, args.label.as_deref())? {
        planned.insert(
            entry.target.clone(),
            PruneItemReport {
                target: entry.target,
                reasons: vec!["stale-exclusive-holder".to_owned()],
                action: "delete-exclusive".to_owned(),
                result: None,
            },
        );
    }
    Ok(planned.into_values().collect())
}

fn apply(
    context: &mut Context,
    args: &Prune,
    items: &mut [PruneItemReport],
) -> Result<Vec<Notice>, CoreError> {
    let mut notices = Vec::new();
    if args.records.is_some() {
        let target = context.resolve(Some(&items[0].target))?;
        let tree = context.tree(&target)?;
        let replaced = matches!(
            context.phase(&tree, context.read_state(&target)?.as_ref())?,
            DerivedPhase::Replaced
        );
        let records = executor::newest_resources_first(
            context
                .read_state(&target)?
                .map(|state| state.resources.into_values().collect::<Vec<_>>())
                .unwrap_or_default(),
        );
        let total = records.len();
        let arenas = executor::arena_snapshot(context, &target.label)?;
        for record in records {
            let holder = context.holder(target.to_string(), "prune")?;
            let _resource = lock::resource(
                &executor::resource_lock_path(context, &record.key),
                &holder,
                super::context::duration(
                    context.settings.locks.resource.as_deref(),
                    Duration::from_secs(120),
                ),
            )?;
            if let Ok(result) =
                executor::destroy_stored_resource(context, &target, record, replaced, &arenas)
            {
                notices.extend(result.notice);
            }
        }
        let remaining = context
            .read_state(&target)?
            .map(|state| state.resources.len())
            .unwrap_or_default();
        let label = context.registry.labels[&tree.label].clone();
        let git_holder = context.holder(target.to_string(), "prune")?;
        let _git = lock::git(
            &context.git_lock_path(&label.gitdir_id),
            &git_holder,
            super::context::duration(
                context.settings.locks.repo_git.as_deref(),
                Duration::from_secs(60),
            ),
        )?;
        context
            .git(Path::new(label.path.as_str()))?
            .worktree_prune()?;
        items[0].result = Some(serde_json::json!({
            "records": total,
            "remaining": remaining,
        }));
        return Ok(notices);
    }

    let exclusive_entries = stale_exclusive_entries(context, args.label.as_deref())?
        .into_iter()
        .map(|entry| (entry.target.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    for item in items.iter_mut() {
        match item.action.as_str() {
            "remove" => {
                let output = remove::run(
                    context,
                    Remove {
                        target: item.target.clone(),
                        force: true,
                        delete_branch: false,
                        keep_branch: true,
                        keep_orphans: false,
                        wait: None,
                    },
                )?;
                notices.extend(output.notices);
                item.result = Some(output.data);
            }
            "destroy-records" => {
                let target = context.resolve(Some(&item.target))?;
                let tree = context.tree(&target)?;
                let replaced = matches!(
                    context.phase(&tree, context.read_state(&target)?.as_ref())?,
                    DerivedPhase::Replaced
                );
                let records = executor::newest_resources_first(
                    context
                        .read_state(&target)?
                        .map(|state| state.resources.into_values().collect::<Vec<_>>())
                        .unwrap_or_default(),
                );
                let total = records.len();
                let arenas = executor::arena_snapshot(context, &target.label)?;
                for record in records {
                    if let Ok(result) = executor::destroy_stored_resource(
                        context, &target, record, replaced, &arenas,
                    ) {
                        notices.extend(result.notice);
                    }
                }
                let remaining = context
                    .read_state(&target)?
                    .map(|state| state.resources.len())
                    .unwrap_or_default();
                item.result = Some(serde_json::json!({"records": total, "remaining": remaining}));
            }
            "delete-state" => {
                wt_sys::fsx::remove_path(Path::new(&item.target))?;
                item.result = Some(serde_json::json!({"deleted": true}));
            }
            "delete-cache" => {
                // Planned paths are always inside the cache root; the
                // contained delete re-derives that instead of trusting the
                // report string, and never follows symlinks.
                let cache_root = context.cache_root();
                let deleted = Path::new(&item.target)
                    .strip_prefix(&cache_root)
                    .ok()
                    .and_then(|relative| relative.to_str())
                    .and_then(|relative| wt_core::model::RelPath::new(relative).ok())
                    .map(|relative| wt_sys::fsx::remove_dir_all_nofollow(&cache_root, &relative))
                    .transpose()?
                    .unwrap_or(false);
                if let Some(parent) = Path::new(&item.target).parent() {
                    let _ = wt_sys::fsx::remove_empty_dir(parent);
                }
                item.result = Some(serde_json::json!({"deleted": deleted}));
            }
            "delete-exclusive" => {
                let deleted = if let Some(entry) = exclusive_entries.get(&item.target) {
                    collect_stale_exclusive(context, entry)?
                } else {
                    false
                };
                item.result = Some(serde_json::json!({"deleted": deleted}));
            }
            "keep" => item.result = Some(serde_json::json!({"kept": true})),
            "collect" => {}
            _ => unreachable!("prune actions are closed"),
        }
    }

    let labels = context
        .registry
        .labels
        .keys()
        .filter(|label| {
            args.label
                .as_deref()
                .is_none_or(|value| label.as_str() == value)
        })
        .cloned()
        .collect::<Vec<_>>();
    for label in &labels {
        let record = context.registry.labels[label].clone();
        let holder = context.holder(label.to_string(), "prune")?;
        let _git = lock::git(
            &context.git_lock_path(&record.gitdir_id),
            &holder,
            super::context::duration(
                context.settings.locks.repo_git.as_deref(),
                Duration::from_secs(60),
            ),
        )?;
        context
            .git(Path::new(record.path.as_str()))?
            .worktree_prune()?;
    }

    let tombstones = context
        .registry
        .tombstones
        .iter()
        .filter(|tombstone| {
            args.label
                .as_deref()
                .is_none_or(|label| tombstone.label.as_str() == label)
        })
        .cloned()
        .collect::<Vec<_>>();
    for tombstone in &tombstones {
        close_tombstone_session(context, &tombstone.session_name())?;
    }
    if !tombstones.is_empty() {
        let holder = context.holder("prune", "prune")?;
        let removed = tombstones
            .iter()
            .map(|record| (record.label.clone(), record.name.clone()))
            .collect::<BTreeSet<_>>();
        context.mutate_registry(&holder, |registry| {
            registry
                .tombstones
                .retain(|record| !removed.contains(&(record.label.clone(), record.name.clone())));
            Ok(())
        })?;
        for label in tombstones
            .iter()
            .map(|record| record.label.clone())
            .collect::<BTreeSet<_>>()
        {
            door::recompute_exclude(context, &label)?;
        }
    }
    for item in items.iter_mut().filter(|item| item.action == "collect") {
        item.result = Some(serde_json::json!({"collected": true}));
    }
    Ok(notices)
}

#[derive(Clone)]
struct StaleExclusiveEntry {
    target: String,
    path: PathBuf,
    lock_path: PathBuf,
    key: String,
}

fn stale_exclusive_entries(
    context: &Context,
    label_filter: Option<&str>,
) -> Result<Vec<StaleExclusiveEntry>, CoreError> {
    let mut stores = context
        .registry
        .labels
        .keys()
        .filter(|label| label_filter.is_none_or(|filter| label.as_str() == filter))
        .map(|label| {
            (
                label.to_string(),
                context.home.join(wt_core::model::repo_state_path(label)),
                context.home.join(format!("locks/{label}/_repo.rmw.lock")),
            )
        })
        .collect::<Vec<_>>();
    stores.push((
        "_machine".to_owned(),
        context.home.join(wt_core::model::machine_state_path()),
        context.home.join("locks/_machine.rmw.lock"),
    ));

    let mut entries = Vec::new();
    for (store, path, lock_path) in stores {
        let Some(state) = wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")? else {
            continue;
        };
        for (key, holder) in state.exclusive {
            if !arena_holder_is_live(context, &holder.holder) {
                entries.push(StaleExclusiveEntry {
                    target: format!("{store}:exclusive.{key}"),
                    path: path.clone(),
                    lock_path: lock_path.clone(),
                    key,
                });
            }
        }
    }
    entries.sort_by(|left, right| left.target.cmp(&right.target));
    Ok(entries)
}

fn collect_stale_exclusive(
    context: &Context,
    entry: &StaleExclusiveEntry,
) -> Result<bool, CoreError> {
    let holder = context.holder(entry.target.clone(), "prune")?;
    let _lock = lock::state_rmw(&entry.lock_path, &holder, context.rmw_wait())?;
    let Some(mut state) = wt_sys::fsx::read_json::<RepoState>(&entry.path, "STATE_CORRUPT")? else {
        return Ok(false);
    };
    let stale = state
        .exclusive
        .get(&entry.key)
        .is_some_and(|holder| !arena_holder_is_live(context, &holder.holder));
    if !stale {
        return Ok(false);
    }
    state.exclusive.remove(&entry.key);
    wt_sys::fsx::write_json(&entry.path, &state)?;
    Ok(true)
}

fn arena_holder_is_live(context: &Context, holder: &str) -> bool {
    wt_core::model::Target::parse(holder).is_ok_and(|target| {
        context
            .registry
            .trees
            .iter()
            .any(|tree| tree.label == target.label && tree.name == target.name)
    })
}

fn state_orphans(
    context: &Context,
    label_filter: Option<&str>,
) -> Result<Vec<std::path::PathBuf>, CoreError> {
    let mut output = Vec::new();
    for label_dir in wt_sys::fsx::read_dir_paths(&context.home.join("state"))? {
        if label_dir.file_name().and_then(|value| value.to_str()) == Some("_machine.json")
            || !matches!(
                wt_sys::fsx::path_kind(&label_dir)?,
                wt_sys::fsx::PathKind::Directory
            )
        {
            continue;
        }
        let Some(label) = label_dir.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if label_filter.is_some_and(|filter| filter != label) {
            continue;
        }
        for path in wt_sys::fsx::read_dir_paths(&label_dir)? {
            let Some(name) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if name == "_repo" {
                continue;
            }
            let live = context
                .registry
                .trees
                .iter()
                .any(|tree| tree.label.as_str() == label && tree.name == name);
            if !live {
                output.push(path);
            }
        }
    }
    output.sort();
    Ok(output)
}

/// Build-cache entries under `$WT_HOME/cache/cargo-build` that belong to no
/// registered tree: an unregistered label's whole directory, and any entry
/// under a registered label that is not a live or tombstoned `name_short`.
/// The latter also covers migration from the retired per-repository layout,
/// whose `debug/`-style contents can never match a `name_short`. Tombstoned
/// names are kept because recreating the same address reuses its
/// coordinates, so the cache would be adopted warm.
pub(crate) fn cache_orphans(
    context: &Context,
    label_filter: Option<&str>,
) -> Result<Vec<std::path::PathBuf>, CoreError> {
    let mut output = Vec::new();
    let root = context.cache_root().join("cargo-build");
    if !matches!(
        wt_sys::fsx::path_kind(&root)?,
        wt_sys::fsx::PathKind::Directory
    ) {
        return Ok(output);
    }
    for label_dir in wt_sys::fsx::read_dir_paths(&root)? {
        let Some(label) = label_dir.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if label_filter.is_some_and(|filter| filter != label) {
            continue;
        }
        if !context
            .registry
            .labels
            .keys()
            .any(|known| known.as_str() == label)
        {
            output.push(label_dir);
            continue;
        }
        if !matches!(
            wt_sys::fsx::path_kind(&label_dir)?,
            wt_sys::fsx::PathKind::Directory
        ) {
            continue;
        }
        for path in wt_sys::fsx::read_dir_paths(&label_dir)? {
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let claimed = context
                .registry
                .trees
                .iter()
                .map(|tree| (tree.label.as_str(), tree.name_short()))
                .chain(
                    context
                        .registry
                        .tombstones
                        .iter()
                        .map(|tombstone| (tombstone.label.as_str(), tombstone.name_short())),
                )
                .any(|(owner, name_short)| owner == label && name_short == name);
            if !claimed {
                output.push(path);
            }
        }
    }
    output.sort();
    Ok(output)
}

fn close_tombstone_session(context: &Context, session: &str) -> Result<(), CoreError> {
    if context.settings.session.backend == wt_core::settings::SessionBackend::None {
        return Ok(());
    }
    let timeout = wt_core::model::duration_millis(&context.settings.session.tmux_timeout)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(10));
    let tmux = wt_sys::tmux::Tmux::new("tmux", timeout);
    match tmux.has_session(session) {
        Ok(true) => tmux.kill_session(session),
        Ok(false) => Ok(()),
        Err(error) if error.code.0 == "TOOL_MISSING" => Ok(()),
        Err(error) => Err(error),
    }
}
