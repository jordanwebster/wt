use std::ffi::OsString;

use wt_core::{CoreError, ExitClass};
use wt_sys::proc::CommandRequest;

use crate::cli::Exec;

use super::{door, Context, Output};

pub(crate) fn run(context: &mut Context, args: Exec) -> Result<Output, CoreError> {
    if context.json {
        return Err(json_unsupported());
    }
    if args.no_gate && !context.parent_env.contains_key("TMUX") {
        return Err(CoreError::new(
            ExitClass::Usage,
            "NO_GATE_REFUSED",
            "--no-gate is only valid inside tmux",
            "sessions are started by `wt open`",
        ));
    }
    let mut door = door::enter(context, args.target.as_deref(), "exec", args.force_env)?;
    door.emit_notices(context);
    let mut request = CommandRequest::new(&args.cmd[0]);
    request.args = args.cmd[1..].iter().map(OsString::from).collect();
    request.cwd = Some(door.cwd.clone());
    request.env = door.env.env.clone();
    request.clear_env = true;
    if args.no_gate {
        door.release_gate();
        wt_sys::proc::execvp_inheriting(&request, &[])?;
    } else {
        wt_sys::proc::execvp_inheriting(&request, &door.inherited_fds())?;
    }
    unreachable!("execvp returns only on failure")
}

pub(crate) fn json_unsupported() -> CoreError {
    CoreError::new(
        ExitClass::Usage,
        "JSON_UNSUPPORTED",
        "passthrough doors have no JSON envelope",
        "use `wt env --json` or `wt run --json`",
    )
}
