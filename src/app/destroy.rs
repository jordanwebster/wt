use wt_core::report::ResourceActionData;
use wt_core::{CoreError, ExitClass};

use crate::cli::ResourceAction;

use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: ResourceAction) -> Result<Output, CoreError> {
    if !context.confirm(&format!("destroy resource {}", args.task))? {
        return Output::data(serde_json::json!({"destroyed": false}));
    }
    let door = door::enter(context, args.target.as_deref(), "destroy")?;
    let notices = door.notices.clone();
    let plan = executor::plan(context, &door, &args.task)?;
    let node = plan
        .nodes
        .iter()
        .find(|node| node.id == args.task)
        .ok_or_else(|| resource_required(&args.task))?;
    let key = node
        .resource
        .as_ref()
        .ok_or_else(|| resource_required(&args.task))?;
    executor::refresh_all_declarations(context, &door)?;
    let before =
        executor::resource_state(context, &door, key)?.unwrap_or_else(|| "declared".to_owned());
    let (after, child) = executor::destroy_resource(context, &door, key, false)?;
    Ok(Output::data(ResourceActionData {
        target: door.target.to_string(),
        scope: key.scope.to_string(),
        task: key.task.clone(),
        before,
        after,
        child,
    })?
    .with_notices(notices))
}

pub(crate) fn resource_required(task: &str) -> CoreError {
    CoreError::new(
        ExitClass::Usage,
        "NOT_A_RESOURCE",
        format!("task `{task}` is not a resource"),
        "choose a task with exists, destroy, and tied_to",
    )
}
