use std::ffi::OsString;
use std::path::Path;

use wt_core::config::Command;
use wt_core::{CoreError, ExitClass};
use wt_sys::proc::CommandRequest;

use crate::cli::Edit;

use super::{door, exec, Context, Output};

pub(crate) fn run(context: &mut Context, args: Edit) -> Result<Output, CoreError> {
    if context.json {
        return Err(exec::json_unsupported());
    }
    let door = door::enter(context, args.target.as_deref(), "edit")?;
    door.emit_notices(context);
    let (command, templated) = context
        .settings
        .editor
        .as_ref()
        .cloned()
        .map(|command| (command, true))
        .or_else(|| inherited_editor(&door.env.env, "VISUAL").map(|command| (command, false)))
        .or_else(|| inherited_editor(&door.env.env, "EDITOR").map(|command| (command, false)))
        .ok_or_else(|| {
            CoreError::new(
                ExitClass::State,
                "EDITOR_UNSET",
                "no editor is configured",
                "set the `editor` settings key in `$WT_HOME/config.toml`, or set $VISUAL or $EDITOR",
            )
        })?;
    let template = wt_core::template::Context {
        vars: &door.env.vars,
        functions: &door.env.functions,
    };
    let (program, arguments) = match command {
        Command::Argv(argv) => {
            let mut argv = argv
                .iter()
                .map(|value| {
                    if templated {
                        wt_core::template::expand(value, &template).map(OsString::from)
                    } else {
                        Ok(OsString::from(value))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let program = argv.remove(0);
            (program, argv)
        }
        Command::Shell(shell) => {
            let shell = if templated {
                wt_core::template::expand(&shell, &template)?
            } else {
                shell
            };
            (
                OsString::from("sh"),
                vec![OsString::from("-c"), OsString::from(shell)],
            )
        }
    };
    let mut request = CommandRequest::new(program);
    request.args = arguments;
    request.cwd = Some(Path::new(door.tree.path.as_str()).to_path_buf());
    request.env = door.env.env.clone();
    request.clear_env = true;
    wt_sys::proc::execvp_inheriting(&request, &door.inherited_fds())?;
    unreachable!("execvp returns only on failure")
}

fn inherited_editor(
    env: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Option<Command> {
    env.get(name)
        .filter(|value| !value.is_empty())
        .cloned()
        .map(Command::Shell)
}
