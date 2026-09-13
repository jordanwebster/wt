use wt_core::report::ResourceActionData;
use wt_core::{CoreError, ExitClass};

use crate::cli::ResourceAction;

use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: ResourceAction) -> Result<Output, CoreError> {
    if !context.confirm(&format!("destroy resource {}", args.resource))? {
        return Output::data(serde_json::json!({"destroyed": false}));
    }
    let door = door::enter(context, args.target.as_deref(), "destroy")?;
    let mut notices = door.notices.clone();
    let plan = executor::plan(context, &door, &args.resource)?;
    let node = plan
        .nodes
        .iter()
        .find(|node| node.id == args.resource)
        .ok_or_else(|| resource_required(&args.resource))?;
    let key = node
        .resource
        .as_ref()
        .ok_or_else(|| resource_required(&args.resource))?;
    let pool_slot = args
        .resource
        .rsplit_once('#')
        .and_then(|(name, index)| index.parse::<u16>().ok().map(|index| (name, index)));
    let pool_keep = if let Some((name, index)) = pool_slot {
        let scope = wt_core::config::effective_scope(&door.config, ".")?;
        let pool = scope
            .resources
            .get(name)
            .ok_or_else(|| resource_required(name))?;
        let dir = super::with_resources::lease_dir(context, &door, name, pool.tied_to);
        let slot = index.saturating_sub(1);
        let occupancy = wt_sys::lock::named_occupancy(&dir, pool.count.unwrap_or(1))?;
        let keep_path = super::leases::keep_path(&dir, slot);
        let keep = super::leases::active_keep(&keep_path)?;
        if occupancy.holders.iter().any(|holder| holder.slot == slot) || keep.is_some() {
            return Err(CoreError::new(
                ExitClass::Conflict,
                "LEASE_HELD",
                format!("resource `{}` currently has an active hold", args.resource),
                "wait for the lease or keep to end",
            ));
        }
        Some(keep_path)
    } else {
        None
    };
    executor::refresh_all_declarations(context, &door)?;
    let before =
        executor::resource_state(context, &door, key)?.unwrap_or_else(|| "declared".to_owned());
    let arenas = executor::arena_snapshot(context, &door.tree.label)?;
    let result = executor::destroy_resource(context, &door, key, false, &arenas)?;
    if let Some(path) = pool_keep {
        super::leases::clear_keep(&path)?;
    }
    if let Some(notice) = result.notice {
        notices.push(notice);
    }
    Ok(Output::data(ResourceActionData {
        target: door.target.to_string(),
        scope: key.scope.to_string(),
        resource: key.resource.clone(),
        before,
        after: result.state,
        child: result.child,
    })?
    .with_notices(notices))
}

pub(crate) fn resource_required(resource: &str) -> CoreError {
    CoreError::new(
        ExitClass::Usage,
        "NOT_A_RESOURCE",
        format!("`{resource}` is not a durable resource"),
        "choose a resource with exists, destroy, and tied_to",
    )
}
