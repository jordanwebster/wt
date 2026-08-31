use wt_core::CoreError;

use crate::cli::Run;

use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: Run) -> Result<Output, CoreError> {
    let door = door::enter(context, args.target.as_deref(), "run")?;
    let notices = door.notices.clone();
    let plan = executor::plan(context, &door, &args.task)?;
    let args_target = executor::validate_args(&plan, &args.args)?;
    executor::validate_take(&plan, args.take)?;
    if args.dry_run {
        return Ok(Output::data(executor::dry_run(&plan, &args.args))?.with_notices(notices));
    }
    let execution = executor::execute_plan(
        context,
        &door,
        &plan,
        args_target
            .as_deref()
            .map(|target| (target, args.args.as_slice())),
        executor::ExecuteOptions {
            timeout: args.timeout.as_deref(),
            wait: args.wait.as_deref(),
            no_log: args.no_log,
            take: args.take,
        },
    )?;
    let data = execution.data;
    let notices = notices.into_iter().chain(execution.notices);
    if context.json {
        Ok(Output::data(data)?.with_notices(notices))
    } else {
        let text = data
            .displaced
            .as_ref()
            .map_or_else(String::new, |target| format!("displaced {target}\n"));
        Ok(Output::text(data, text)?.with_notices(notices))
    }
}
