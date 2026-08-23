use std::ffi::OsString;

use wt_core::CoreError;
use wt_sys::proc::CommandRequest;

use crate::cli::ShellDoor;

use super::{door, exec, Context, Output};

pub(crate) fn run(context: &mut Context, args: ShellDoor) -> Result<Output, CoreError> {
    if context.json {
        return Err(exec::json_unsupported());
    }
    let door = door::enter(context, args.target.as_deref(), "shell", args.force_env)?;
    door.emit_notices(context);
    let program = context
        .settings
        .shell
        .program
        .as_ref()
        .map(|path| path.as_str().to_owned())
        .or_else(|| context.parent_env.get("SHELL").cloned())
        .unwrap_or_else(|| "/bin/sh".to_owned());
    eprintln!(
        "wt: entering {} (WT_BIN={})",
        door.target,
        door.env.env.get("WT_BIN").map(String::as_str).unwrap_or("")
    );
    let mut request = CommandRequest::new(program);
    request.args = vec![OsString::from("-i")];
    request.cwd = Some(door.cwd.clone());
    request.env = door.env.env.clone();
    request.clear_env = true;
    wt_sys::proc::execvp_inheriting(&request, &door.inherited_fds())?;
    unreachable!("execvp returns only on failure")
}
