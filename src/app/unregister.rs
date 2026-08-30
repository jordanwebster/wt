use std::path::Path;
use wt_core::lifecycle::{OpVerb, Operation, RepoState, StatePhase};
use wt_core::report::{ArtifactReport, DestroyedReport, UnregisterData};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, Mode};

use crate::cli::{Remove, Unregister};

use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: Unregister) -> Result<Output, CoreError> {
    let label = wt_core::model::Label::new(&args.label)?;
    if !context.registry.labels.contains_key(&label) {
        return Output::data(UnregisterData {
            label: label.to_string(),
            unregistered: false,
            destroyed: Vec::new(),
            artifacts: Vec::new(),
        });
    }
    let mut linked = context
        .registry
        .trees
        .iter()
        .filter(|tree| tree.label == label && !tree.canonical)
        .cloned()
        .collect::<Vec<_>>();
    if !linked.is_empty() && !args.force {
        return Err(CoreError::new(
            ExitClass::State,
            "TREES_EXIST",
            "linked trees still exist for the label",
            "remove them first or retry unregister with --force",
        ));
    }
    let targets = context
        .registry
        .trees
        .iter()
        .filter(|tree| tree.label == label)
        .map(super::context::target_of)
        .map(|target| target.to_string())
        .collect::<Vec<_>>();
    if !context.confirm(&format!("unregister plan: {}", targets.join(", ")))? {
        return Output::data(UnregisterData {
            label: label.to_string(),
            unregistered: false,
            destroyed: Vec::new(),
            artifacts: Vec::new(),
        });
    }
    if args.force {
        let previous_yes = context.yes;
        context.yes = true;
        for tree in linked.drain(..) {
            super::remove::run(
                context,
                Remove {
                    target: super::context::target_of(&tree).to_string(),
                    force: true,
                    delete_branch: false,
                    keep_branch: true,
                    keep_orphans: false,
                    wait: None,
                },
            )?;
            context.reload_registry()?;
        }
        context.yes = previous_yes;
    }
    let target = wt_core::model::Target::canonical(label.clone());
    let tree = context.tree(&target)?;
    let session_closed = super::close::close_tree(context, &tree).unwrap_or(false);
    let holder = context.holder(target.to_string(), "unregister")?;
    let _tree = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    context.require_identity(&tree)?;
    let now = wt_sys::fsx::timestamp()?;
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Removing;
        state.op = Some(Operation {
            verb: OpVerb::Remove,
            pid: std::process::id(),
            started: now,
        });
        Ok(())
    })?;
    let door = door::enter_held(context, tree.clone(), "unregister", false)?;
    executor::refresh_lifecycle_declarations(context, &door)?;
    let mut destroyed = Vec::new();
    let mut teardown_errors = Vec::new();
    let tree_records = executor::newest_resources_first(
        context
            .read_state(&target)?
            .map(|state| state.resources.into_values().collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    for record in tree_records {
        let key = record.key.clone();
        match executor::destroy_resource(context, &door, &key, true) {
            Ok((state, child)) => destroyed.push(DestroyedReport {
                scope: key.scope.to_string(),
                task: key.task,
                state,
                child,
            }),
            Err(error) => {
                teardown_errors.push(error);
                destroyed.push(DestroyedReport {
                    scope: key.scope.to_string(),
                    task: key.task,
                    state: "orphaned".to_owned(),
                    child: None,
                });
            }
        }
    }
    let repo_path = context.home.join(wt_core::model::repo_state_path(&label));
    let repo_records = executor::newest_resources_first(
        wt_sys::fsx::read_json::<RepoState>(&repo_path, "STATE_CORRUPT")?
            .map(|state| state.resources.into_values().collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    for record in repo_records {
        let key = record.key.clone();
        match executor::destroy_resource(context, &door, &key, true) {
            Ok((state, child)) => destroyed.push(DestroyedReport {
                scope: key.scope.to_string(),
                task: key.task,
                state,
                child,
            }),
            Err(error) => {
                teardown_errors.push(error);
                destroyed.push(DestroyedReport {
                    scope: key.scope.to_string(),
                    task: key.task,
                    state: "orphaned".to_owned(),
                    child: None,
                });
            }
        }
    }
    let remaining_tree = context
        .read_state(&target)?
        .is_some_and(|state| !state.resources.is_empty());
    let remaining_repo = wt_sys::fsx::read_json::<RepoState>(&repo_path, "STATE_CORRUPT")?
        .is_some_and(|state| !state.resources.is_empty());
    if remaining_tree || remaining_repo || !teardown_errors.is_empty() {
        return Err(CoreError::new(
            ExitClass::ChildFailed,
            "DESTROY_FAILED",
            "resource records remain after unregister teardown",
            "fix the resource and retry unregister",
        ));
    }
    let state = context.read_state(&target)?;
    let mut artifacts = Vec::new();
    if let Some(state) = &state {
        for item in &state.materialized {
            if item.kind != wt_core::lifecycle::MaterializedKind::Rendered
                || item.path == ".wt"
                || item.path.starts_with(".wt/")
            {
                continue;
            }
            let relative = wt_core::model::RelPath::new(&item.path)?;
            let owned = item.hash.as_ref().is_some_and(|hash| {
                wt_sys::fsx::read_nofollow(Path::new(tree.path.as_str()), &relative)
                    .is_ok_and(|bytes| wt_core::render::content_hash(&bytes) == *hash)
            });
            let action = if owned
                && wt_sys::fsx::remove_nofollow(Path::new(tree.path.as_str()), &relative)?
            {
                "deleted"
            } else {
                "kept"
            };
            artifacts.push(ArtifactReport {
                path: item.path.clone(),
                action: action.to_owned(),
            });
        }
    }
    let wt_dir = Path::new(tree.path.as_str()).join(".wt");
    let deleted = wt_sys::fsx::remove_path(&wt_dir)?;
    artifacts.push(ArtifactReport {
        path: wt_dir.to_string_lossy().into_owned(),
        action: if deleted { "deleted" } else { "kept" }.to_owned(),
    });
    wt_sys::fsx::remove_path(&context.state_path(&target))?;
    wt_sys::fsx::remove_path(&repo_path)?;
    let common = context.registry.labels[&label].common_gitdir.clone();
    context.mutate_registry(&holder, |registry| {
        registry.trees.retain(|record| record.label != label);
        registry.tombstones.retain(|record| record.label != label);
        registry.labels.remove(&label);
        Ok(())
    })?;
    if !artifacts.iter().any(|artifact| artifact.action == "kept") {
        wt_sys::fsx::remove_exclude(&Path::new(common.as_str()).join("info/exclude"))?;
    }
    let _ = session_closed;
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Output::data(UnregisterData {
        label: label.to_string(),
        unregistered: true,
        destroyed,
        artifacts,
    })?
    .with_notices(door.notices))
}
