use wt_core::CoreError;

use crate::cli::Run;

use super::{door, executor, sweep, Context, Output};

pub(crate) fn run(context: &mut Context, args: Run) -> Result<Output, CoreError> {
    let door = door::enter(context, args.target.as_deref(), "run")?;
    let notices = door.notices.clone();
    let plan = executor::plan(context, &door, &args.task)?;
    let args_target = executor::validate_args(&plan, &args.args)?;
    executor::validate_take(&plan, args.take)?;
    if args.dry_run {
        return Ok(
            Output::data(executor::dry_run(&plan, &args.args, args_target.as_deref()))?
                .with_notices(notices),
        );
    }
    // A task that produced build output leaves superseded units behind
    // whether it finished or failed, so the sweep follows either way (SPEC
    // §11.9); on failure its notices ride on the error envelope. `build`
    // always sweeps; any other task sweeps when the build directories
    // changed since the last sweep.
    let always = args.task == "build";
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
    );
    context.last_sweep = None;
    let swept = sweep::after_task(context, &door.tree, always);
    let execution = match execution {
        Ok(execution) => execution,
        Err(error) => {
            context.pending_notices.extend(swept);
            return Err(error);
        }
    };
    let data = execution.data;
    let notices = notices.into_iter().chain(execution.notices).chain(swept);
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
