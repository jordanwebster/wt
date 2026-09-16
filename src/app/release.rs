use std::time::Duration;

use wt_core::lifecycle::{OpVerb, Operation, StatePhase};
use wt_core::report::{DestroyedReport, Notice, NoticeLevel, ReleaseData};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, Mode};

use crate::cli::Release;

use super::{door, executor, forget, leases, remove, Context, Output};

pub(crate) fn run(context: &mut Context, args: Release) -> Result<Output, CoreError> {
    let target = match context.resolve(args.target.as_deref()) {
        Ok(target) => target,
        Err(error) if error.code.0 == "NOT_FOUND" => {
            if let Some(target) = released_target(context, args.target.as_deref()) {
                return Ok(Output::data(ReleaseData {
                    target: target.to_string(),
                    released: false,
                    destroyed: Vec::new(),
                    artifacts: Vec::new(),
                    session_closed: false,
                })?
                .with_notices([Notice {
                    level: NoticeLevel::Info,
                    code: "ALREADY_RELEASED".to_owned(),
                    subject: Some(target.to_string()),
                    message: format!("{target} was already released; nothing changed"),
                }]));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let tree = context.tree(&target)?;
    if tree.canonical {
        return Err(CoreError::new(
            ExitClass::Usage,
            "USE_UNREGISTER",
            "canonical checkouts are unregistered, not released",
            "use `wt unregister <label>`",
        ));
    }
    context.require_identity(&tree)?;
    let session_closed = if remove::tmux_has(context, &tree)? {
        super::close::close_tree(context, &tree)?
    } else {
        false
    };
    let holder = context.holder(target.to_string(), "release")?;
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
    let now = wt_sys::fsx::timestamp()?;
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Removing;
        state.op = Some(Operation {
            verb: OpVerb::Release,
            pid: std::process::id(),
            started: now.clone(),
        });
        Ok(())
    })?;
    let entered = door::enter_held(context, tree.clone(), "release", false)?;
    executor::refresh_lifecycle_declarations(context, &entered)?;
    leases::release_tree(context, &target.to_string())?;

    let arenas = executor::arena_snapshot(context, &target.label)?;
    let records = executor::newest_resources_first(
        context
            .read_state(&target)?
            .map(|state| state.resources.into_values().collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    let mut destroyed = Vec::new();
    let mut notices = entered.notices.clone();
    let mut first_error = None;
    for record in records {
        let key = record.key.clone();
        let resource_holder = context.holder(target.to_string(), "release")?;
        let _resource = lock::resource(
            &executor::resource_lock_path(context, &key),
            &resource_holder,
            super::context::duration(
                context.settings.waits.resource.as_deref(),
                Duration::from_secs(120),
            ),
        )?;
        match executor::destroy_resource(context, &entered, &key, true, &arenas) {
            Ok(result) => {
                if let Some(notice) = result.notice {
                    notices.push(notice);
                }
                destroyed.push(DestroyedReport {
                    scope: key.scope.to_string(),
                    resource: key.resource,
                    state: result.state,
                    child: result.child,
                });
            }
            Err(error) => {
                first_error.get_or_insert(error);
                destroyed.push(DestroyedReport {
                    scope: key.scope.to_string(),
                    resource: key.resource,
                    state: "orphaned".to_owned(),
                    child: None,
                });
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    if context
        .read_state(&target)?
        .is_some_and(|state| !state.resources.is_empty())
    {
        return Err(CoreError::new(
            ExitClass::State,
            "RESOURCE_ORPHANED",
            format!("{target} still has resource records after teardown"),
            "repair or destroy the remaining resources, then retry",
        ));
    }
    let artifacts = forget::detach(context, &tree, &target, &holder, "released", false)?;
    Ok(Output::data(ReleaseData {
        target: target.to_string(),
        released: true,
        destroyed,
        artifacts,
        session_closed,
    })?
    .with_notices(notices))
}

fn released_target(context: &Context, address: Option<&str>) -> Option<wt_core::model::Target> {
    let requested_path =
        address.and_then(|address| wt_sys::fsx::canonicalize(std::path::Path::new(address)).ok());
    context.registry.tombstones.iter().find_map(|tombstone| {
        if tombstone.reason != "released" {
            return None;
        }
        let target = wt_core::model::Target {
            label: tombstone.label.clone(),
            name: tombstone.name.clone(),
        };
        let path = std::path::Path::new(tombstone.path.as_str());
        let matches = address.is_some_and(|address| address == target.to_string())
            || requested_path
                .as_ref()
                .is_some_and(|requested| requested == path)
            || (address.is_none() && context.cwd.starts_with(path));
        matches.then_some(target)
    })
}
