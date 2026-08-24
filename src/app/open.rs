use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use wt_core::config::Command;
use wt_core::model::TreeRec;
use wt_core::report::{Notice, OpenSessionReport, SessionReport, SessionsData};
use wt_core::settings::SessionBackend;
use wt_core::{CoreError, ExitClass};

use crate::cli::Open;

use super::{door, shell, AfterRender, Context, Output};

pub(crate) fn run(context: &mut Context, args: Open) -> Result<Output, CoreError> {
    require_tmux_backend(context)?;
    let trees = if args.all {
        context.registry.trees.clone()
    } else {
        let target = context.resolve(args.target.as_deref())?;
        vec![context.tree(&target)?]
    };
    let (sessions, notices) = open_trees(context, trees, args.agent.as_deref())?;
    let attach = (!args.all && should_attach(context, args.no_attach))
        .then(|| sessions.first().map(|session| session.name.clone()))
        .flatten();
    let mut output = Output::data(SessionsData {
        sessions: sessions.into_iter().map(SessionReport::Open).collect(),
    })?
    .with_notices(notices);
    if let Some(session) = attach {
        output = output.after_render(AfterRender::Attach { session });
    }
    Ok(output)
}

pub(crate) fn provision_new(context: &mut Context, target: &str) -> Result<Vec<Notice>, CoreError> {
    if context.settings.session.backend == SessionBackend::None {
        return Ok(Vec::new());
    }
    let target = context.resolve(Some(target))?;
    let tree = context.tree(&target)?;
    let (_, notices) = open_trees(context, vec![tree], None)?;
    Ok(notices)
}

pub(crate) fn open_new_after_summary(context: &mut Context, target: &str) -> Result<(), CoreError> {
    let target = context.resolve(Some(target))?;
    let tree = context.tree(&target)?;
    let (sessions, _) = open_trees(context, vec![tree], None)?;
    let session = sessions
        .first()
        .expect("opening one tree produces one session");
    attach(context, &session.name)
}

pub(crate) fn should_attach(context: &Context, no_attach: bool) -> bool {
    context.settings.session.backend == SessionBackend::Tmux
        && context.settings.session.attach
        && context.tty.stdout
        && !context.json
        && !context.parent_env.contains_key("WT_ACTIVATION")
        && !no_attach
}

pub(crate) fn attach(context: &Context, session: &str) -> Result<(), CoreError> {
    let tmux = tmux(context);
    if context.parent_env.contains_key("TMUX") {
        tmux.switch_client(session)
    } else {
        tmux.attach_session(session)
    }
}

pub(crate) fn require_tmux_backend(context: &Context) -> Result<(), CoreError> {
    if context.settings.session.backend == SessionBackend::None {
        return Err(CoreError::new(
            ExitClass::State,
            "SESSION_DISABLED",
            "`session.backend` is `none`",
            "set `session.backend = \"tmux\"` in `$WT_HOME/config.toml`",
        ));
    }
    Ok(())
}

fn open_trees(
    context: &mut Context,
    trees: Vec<TreeRec>,
    agent_override: Option<&str>,
) -> Result<(Vec<OpenSessionReport>, Vec<Notice>), CoreError> {
    let tmux = tmux(context);
    let mut sessions = Vec::new();
    let mut notices = Vec::new();
    for tree in trees {
        let target = super::context::target_of(&tree);
        let mut gate = door::enter(context, Some(&target.to_string()), "open", false)?;
        notices.extend(gate.notices.clone());
        let mut existing = tmux.has_session(&tree.session_name)?;
        let mut created = false;
        let mut reported_agent = tree.agent.clone();
        if !existing {
            let launch = launch_command(context, &tree, agent_override)?;
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
            inner.extend(launch.argv);
            if let Err(error) =
                tmux.new_session(&tree.session_name, Path::new(tree.path.as_str()), &inner)
            {
                if !tmux.has_session(&tree.session_name)? {
                    return Err(error);
                }
                existing = true;
            } else {
                created = true;
                reported_agent.clone_from(&launch.agent);
                if let Some(agent) = launch.record_agent {
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
                }
            }
        }
        gate.release_gate();
        sessions.push(OpenSessionReport {
            target: target.to_string(),
            name: tree.session_name,
            created,
            existing,
            agent: reported_agent,
            foreground: false,
        });
    }
    Ok((sessions, notices))
}

struct Launch {
    argv: Vec<OsString>,
    agent: Option<String>,
    record_agent: Option<String>,
}

fn launch_command(
    context: &Context,
    tree: &TreeRec,
    agent_override: Option<&str>,
) -> Result<Launch, CoreError> {
    if let Some(agent) = agent_override {
        return agent_launch(context, agent, false, true);
    }
    if let Some(agent) = tree.agent.as_deref() {
        return agent_launch(context, agent, true, false);
    }
    if let Some(agent) = context.settings.session.agent.as_deref() {
        return agent_launch(context, agent, false, true);
    }
    Ok(Launch {
        argv: shell::command_argv(context),
        agent: None,
        record_agent: None,
    })
}

fn agent_launch(
    context: &Context,
    agent: &str,
    resume: bool,
    record: bool,
) -> Result<Launch, CoreError> {
    let definition = context.settings.agents.get(agent).ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "CONFIG_INVALID",
            format!("agent `{agent}` is not configured"),
            "configure the agent in `$WT_HOME/config.toml`",
        )
    })?;
    let command = if resume {
        &definition.resume
    } else {
        &definition.start
    };
    Ok(Launch {
        argv: command_argv(command),
        agent: Some(agent.to_owned()),
        record_agent: record.then(|| agent.to_owned()),
    })
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

fn tmux(context: &Context) -> wt_sys::tmux::Tmux {
    let timeout = wt_core::model::duration_millis(&context.settings.session.tmux_timeout)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(10));
    wt_sys::tmux::Tmux::new("tmux", timeout)
}
