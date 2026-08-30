use wt_core::report::ResourceActionData;
use wt_core::CoreError;

use crate::cli::ResourceAction;

use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: ResourceAction) -> Result<Output, CoreError> {
    if !context.confirm(&format!("refresh resource {}", args.task))? {
        return Output::data(serde_json::json!({"refreshed": false}));
    }
    let door = door::enter(context, args.target.as_deref(), "refresh")?;
    let notices = door.notices.clone();
    let plan = executor::plan(context, &door, &args.task)?;
    let node = plan
        .nodes
        .iter()
        .find(|node| node.id == args.task)
        .ok_or_else(|| super::destroy::resource_required(&args.task))?;
    let key = node
        .resource
        .as_ref()
        .ok_or_else(|| super::destroy::resource_required(&args.task))?;
    executor::refresh_all_declarations(context, &door)?;
    let before =
        executor::resource_state(context, &door, key)?.unwrap_or_else(|| "declared".to_owned());
    let _ = executor::destroy_resource(context, &door, key, false)?;
    let result = executor::execute_plan(context, &door, &plan, None, None, None, false)?;
    let after =
        executor::resource_state(context, &door, key)?.unwrap_or_else(|| "declared".to_owned());
    Ok(Output::data(ResourceActionData {
        target: door.target.to_string(),
        scope: key.scope.to_string(),
        task: key.task.clone(),
        before,
        after,
        child: result.data.child,
    })?
    .with_notices(notices))
}
