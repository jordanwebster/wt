use wt_core::CoreError;

use crate::cli::Run;

use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: Run) -> Result<Output, CoreError> {
    let door = door::enter(context, args.target.as_deref(), "run")?;
    let notices = door.notices.clone();
    let plan = executor::plan(context, &door, &args.task)?;
    if args.dry_run {
        return Ok(Output::data(executor::dry_run(&plan))?.with_notices(notices));
    }
    let execution = executor::execute_plan(
        context,
        &door,
        &plan,
        args.timeout.as_deref(),
        args.wait.as_deref(),
        args.no_log,
    )?;
    let data = execution.data;
    let notices = notices.into_iter().chain(execution.notices);
    if context.json {
        Ok(Output::data(data)?.with_notices(notices))
    } else {
        Ok(Output::text(data, "")?.with_notices(notices))
    }
}
