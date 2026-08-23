use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use wt_core::config::Command;
use wt_core::report::{OpenSessionReport, SessionReport, SessionsData};
use wt_core::{CoreError, ExitClass};

use crate::cli::Open;

use super::{door, exec, Context, Output};

pub(crate) fn run(context: &mut Context, args: Open) -> Result<Output, CoreError> {
    if context.json && !args.no_attach && !args.all {
        return Err(exec::json_unsupported());
    }
    let trees = if args.all {
        context
            .registry
            .trees
            .iter()
            .filter(|tree| tree.agent.is_some())
            .cloned()
            .collect::<Vec<_>>()
    } else {
        let target = context.resolve(args.target.as_deref())?;
        vec![context.tree(&target)?]
    };
    let timeout = wt_core::model::duration_millis(&context.settings.session.tmux_timeout)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(10));
    let tmux = wt_sys::tmux::Tmux::new("tmux", timeout);
    let tmux_available = match tmux.check_version() {
        Ok(_) => true,
        Err(error) if error.code.0 == "TOOL_MISSING" => false,
        Err(error) => return Err(error),
    };
    let mut sessions = Vec::new();
    let mut notices = Vec::new();
    for tree in trees {
        let target = super::context::target_of(&tree);
        let agent = args
            .agent
            .clone()
            .or_else(|| tree.agent.clone())
            .or_else(|| context.settings.default_agent.clone())
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::State,
                    "CONFIG_INVALID",
                    "no agent was selected",
                    "pass --agent or configure default_agent",
                )
            })?;
        let definition = context
            .settings
            .agents
            .get(&agent)
            .cloned()
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::State,
                    "CONFIG_INVALID",
                    format!("agent `{agent}` is not configured"),
                    "configure the agent in WT_HOME/config.toml",
                )
            })?;
        if !tmux_available {
            if context.tty.stdin && !args.no_attach && !args.all && args.agent.is_some() {
                eprintln!("tmux not found; running `{agent}` in the foreground");
                let mut gate = door::enter(context, Some(&target.to_string()), "open", false)?;
                gate.emit_notices(context);
                context.mutate_registry(
                    &context.holder(target.to_string(), "open")?,
                    |registry| {
                        if let Some(record) = registry
                            .trees
                            .iter_mut()
                            .find(|record| record.tree_id == tree.tree_id)
                        {
                            record.agent = Some(agent.clone());
                        }
                        Ok(())
                    },
                )?;
                gate.release_gate();
                let command = agent_command(&definition.start);
                let request = wt_sys::proc::CommandRequest::expanded(
                    &command,
                    Path::new(tree.path.as_str()),
                    gate.env.env,
                )?;
                wt_sys::proc::execvp_inheriting(&request, &[])?;
                unreachable!("execvp returns only on failure");
            }
            return Err(CoreError::new(
                ExitClass::State,
                "TOOL_MISSING",
                "tmux is required for a detached session",
                "install tmux 3.2 or run attached with --agent",
            ));
        }
        let mut gate = door::enter(context, Some(&target.to_string()), "open", false)?;
        notices.extend(gate.notices.clone());
        let mut existing = tmux.has_session(&tree.session_name)?;
        let mut created = false;
        if !existing {
            let command = if tree.agent.is_some() {
                &definition.resume
            } else {
                &definition.start
            };
            let running_binary = std::env::current_exe().map_err(|error| {
                CoreError::new(
                    ExitClass::Internal,
                    "CURRENT_EXE_FAILED",
                    format!("could not resolve the running wt binary: {error}"),
                    "retry and report this wt bug if it repeats",
                )
            })?;
            let mut inner = vec![
                running_binary.into_os_string(),
                OsString::from("exec"),
                OsString::from("--no-gate"),
                OsString::from(target.to_string()),
                OsString::from("--"),
            ];
            inner.extend(command_argv(command));
            if let Err(error) =
                tmux.new_session(&tree.session_name, Path::new(tree.path.as_str()), &inner)
            {
                if !tmux.has_session(&tree.session_name)? {
                    return Err(error);
                }
                existing = true;
            } else {
                created = true;
            }
            tmux.set_status_left(&tree.session_name, &target.to_string())?;
            context.mutate_registry(&context.holder(target.to_string(), "open")?, |registry| {
                if let Some(record) = registry
                    .trees
                    .iter_mut()
                    .find(|record| record.tree_id == tree.tree_id)
                {
                    record.agent = Some(agent.clone());
                }
                Ok(())
            })?;
        }
        gate.release_gate();
        sessions.push(SessionReport::Open(OpenSessionReport {
            target: target.to_string(),
            name: tree.session_name.clone(),
            created,
            existing,
            agent: Some(agent),
            foreground: false,
        }));
        if !args.no_attach && !args.all {
            if context.parent_env.contains_key("TMUX") {
                tmux.switch_client(&tree.session_name)?;
            } else {
                tmux.attach_session(&tree.session_name)?;
            }
        }
    }
    Ok(Output::data(SessionsData { sessions })?.with_notices(notices))
}

fn command_argv(command: &Command) -> Vec<OsString> {
    match command {
        Command::Argv(argv) => argv.iter().map(OsString::from).collect(),
        Command::Shell(shell) => vec![
            OsString::from("sh"),
            OsString::from("-c"),
            OsString::from(shell),
        ],
    }
}

fn agent_command(command: &Command) -> wt_core::resource::ExpandedCommand {
    match command {
        Command::Argv(argv) => wt_core::resource::ExpandedCommand::Argv { argv: argv.clone() },
        Command::Shell(shell) => wt_core::resource::ExpandedCommand::Shell {
            shell: shell.clone(),
        },
    }
}
