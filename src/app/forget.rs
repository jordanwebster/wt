use std::path::Path;

use wt_core::lifecycle::MaterializedKind;
use wt_core::model::Tombstone;
use wt_core::report::{ArtifactReport, ForgetData};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, Mode};

use crate::cli::Forget;

use super::{door, Context, Output};

pub(crate) fn run(context: &mut Context, args: Forget) -> Result<Output, CoreError> {
    let target = context.resolve(Some(&args.target))?;
    let tree = context.tree(&target)?;
    if tree.canonical {
        return Err(CoreError::new(
            ExitClass::Usage,
            "USE_UNREGISTER",
            "canonical checkouts are unregistered, not forgotten",
            "use `wt unregister <label>`",
        ));
    }
    context.require_identity(&tree)?;
    refuse_resources(context, &tree, &target)?;
    refuse_session(context, &tree)?;
    refuse_holders(context, &target)?;
    if !context.confirm(&format!("forget plan: {target}"))? {
        return not_forgotten(&target);
    }

    let holder = context.holder(target.to_string(), "forget")?;
    let _tree_lock = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )
    .map_err(|error| {
        if error.code.0 == "TREE_IN_USE" {
            CoreError::new(
                ExitClass::Conflict,
                "TREE_IN_USE",
                format!("{target} still has active door holders"),
                "wait for the door holders to exit, then retry",
            )
        } else {
            error
        }
    })?;
    context.require_identity(&tree)?;
    refuse_resources(context, &tree, &target)?;
    refuse_session(context, &tree)?;

    let state = context.read_state(&target)?.ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "STATE_CORRUPT",
            format!("state for `{target}` is missing"),
            "run `wt doctor` and restore or prune the affected record",
        )
    })?;
    let root = Path::new(tree.path.as_str());
    let mut artifacts = Vec::new();
    for item in &state.materialized {
        if item.kind != MaterializedKind::Rendered
            || item.path == ".wt"
            || item.path.starts_with(".wt/")
        {
            continue;
        }
        let relative = wt_core::model::RelPath::new(&item.path)?;
        let owned = item.hash.as_ref().is_some_and(|hash| {
            wt_sys::fsx::read_nofollow(root, &relative)
                .is_ok_and(|bytes| wt_core::render::content_hash(&bytes) == *hash)
        });
        let action = if owned && wt_sys::fsx::remove_nofollow(root, &relative)? {
            "deleted"
        } else {
            "kept"
        };
        artifacts.push(ArtifactReport {
            path: item.path.clone(),
            action: action.to_owned(),
        });
    }
    let wt_dir = root.join(".wt");
    let deleted = wt_sys::fsx::remove_path(&wt_dir)?;
    artifacts.push(ArtifactReport {
        path: wt_dir.to_string_lossy().into_owned(),
        action: if deleted { "deleted" } else { "kept" }.to_owned(),
    });
    wt_sys::fsx::remove_path(&context.state_path(&target))?;
    let tombstone = Tombstone {
        label: tree.label.clone(),
        name: tree.name.clone(),
        slot: tree.slot,
        geometry: tree.geometry,
        ports: tree.ports.clone(),
        path: tree.path.clone(),
        materialized: Vec::new(),
        removed_at: wt_sys::fsx::timestamp()?,
        reason: "forgotten".to_owned(),
    };
    context.mutate_registry(&holder, |registry| {
        registry
            .trees
            .retain(|record| record.tree_id != tree.tree_id);
        registry.tombstones.push(tombstone);
        Ok(())
    })?;
    door::recompute_exclude(context, &tree.label)?;
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Output::data(ForgetData {
        target: target.to_string(),
        forgotten: true,
        artifacts,
    })
}

fn refuse_resources(
    context: &Context,
    tree: &wt_core::model::TreeRec,
    target: &wt_core::model::Target,
) -> Result<(), CoreError> {
    let live = super::executor::instantiated_resources(context, &tree.label, target)?;
    if !live.is_empty() {
        return Err(CoreError::new(
            ExitClass::State,
            "RESOURCES_EXIST",
            format!("{target} still has live resources: {}", live.join(", ")),
            "use `wt destroy` to tear down the resources, or `wt rm` to remove the tree",
        )
        .with_details(serde_json::json!({ "resources": live })));
    }
    Ok(())
}

fn refuse_session(context: &Context, tree: &wt_core::model::TreeRec) -> Result<(), CoreError> {
    if super::remove::tmux_has(context, tree)? {
        return Err(CoreError::new(
            ExitClass::State,
            "SESSION_LIVE",
            format!("session {} is still live", tree.session_name()),
            "run `wt close` for the tree, then retry",
        ));
    }
    Ok(())
}

fn refuse_holders(context: &Context, target: &wt_core::model::Target) -> Result<(), CoreError> {
    let holders = super::remove::door_holders(context, target)?;
    if !holders.is_empty() {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "TREE_IN_USE",
            format!("{target} still has active door holders"),
            "wait for the door holders to exit, then retry",
        )
        .with_details(serde_json::json!({ "holders": holders })));
    }
    Ok(())
}

fn not_forgotten(target: &wt_core::model::Target) -> Result<Output, CoreError> {
    Output::data(ForgetData {
        target: target.to_string(),
        forgotten: false,
        artifacts: Vec::new(),
    })
}
