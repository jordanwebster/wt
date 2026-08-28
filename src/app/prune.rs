use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use wt_core::lifecycle::DerivedPhase;
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
    result?;
    Output::data(PruneData {
        applied: true,
        items,
    })
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
    Ok(planned.into_values().collect())
}

fn apply(
    context: &mut Context,
    args: &Prune,
    items: &mut [PruneItemReport],
) -> Result<(), CoreError> {
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
        for record in records {
            let holder = context.holder(target.to_string(), "prune")?;
            let _resource = lock::resource(
                &resource_lock_path(context, &record.key),
                &holder,
                super::context::duration(
                    context.settings.locks.resource.as_deref(),
                    Duration::from_secs(120),
                ),
            )?;
            let _ = executor::destroy_stored_resource(context, &target, record, replaced);
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
        return Ok(());
    }

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
                for record in records {
                    let _ = executor::destroy_stored_resource(context, &target, record, replaced);
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
        close_tombstone_session(context, &tombstone.session_name)?;
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
    Ok(())
}

fn state_orphans(
    context: &Context,
    label_filter: Option<&str>,
) -> Result<Vec<std::path::PathBuf>, CoreError> {
    let mut output = Vec::new();
    for label_dir in wt_sys::fsx::read_dir_paths(&context.home.join("state"))? {
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

fn resource_lock_path(
    context: &Context,
    key: &wt_core::resource::ResourceKey,
) -> std::path::PathBuf {
    let tied = match key.tied_to {
        wt_core::config::TiedTo::Tree => {
            format!("tree/{}/", key.name.as_deref().unwrap_or("canonical"))
        }
        wt_core::config::TiedTo::Repo => "repo/".to_owned(),
    };
    context.home.join(format!(
        "locks/{}/res/{tied}{}/{}.lock",
        key.label,
        wt_core::model::scope_enc(&key.scope),
        key.task
    ))
}
