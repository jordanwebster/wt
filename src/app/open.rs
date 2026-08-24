use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use wt_core::config::Command;
use wt_core::model::TreeRec;
use wt_core::report::{
    FailedSessionReport, Notice, NoticeLevel, OpenSessionReport, SessionReport, SessionsData,
};
use wt_core::settings::SessionBackend;
use wt_core::{CoreError, ExitClass};

use crate::cli::Open;

use super::{door, shell, AfterRender, Context, Output};

pub(crate) fn run(context: &mut Context, args: Open) -> Result<Output, CoreError> {
    let backend_notice = super::register::resolve_session_backend(context)?;
    require_tmux_backend(context)?;
    let trees = if args.all {
        context.registry.trees.clone()
    } else {
        let target = context.resolve(args.target.as_deref())?;
        vec![context.tree(&target)?]
    };
    let OpenBatch {
        sessions,
        notices,
        failure,
    } = open_trees(context, trees, args.agent.as_deref(), args.all)?;
    let attach = (!args.all && should_attach(context, args.no_attach))
        .then(|| {
            sessions.first().and_then(|session| match session {
                SessionReport::Open(session) => Some(session.name.clone()),
                SessionReport::Closed(_) | SessionReport::Failed(_) => None,
            })
        })
        .flatten();
    let mut output = Output::data(SessionsData { sessions })?.with_notices(notices);
    if let Some(notice) = backend_notice {
        output = output.with_notices([notice]);
    }
    if let Some(error) = failure {
        output = output.with_failure(error);
    }
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
    Ok(open_trees(context, vec![tree], None, false)?.notices)
}

pub(crate) fn open_new_after_summary(context: &mut Context, target: &str) -> Result<(), CoreError> {
    let target = context.resolve(Some(target))?;
    let tree = context.tree(&target)?;
    let batch = open_trees(context, vec![tree], None, false)?;
    let session = batch
        .sessions
        .first()
        .and_then(|session| match session {
            SessionReport::Open(session) => Some(session),
            SessionReport::Closed(_) | SessionReport::Failed(_) => None,
        })
        .expect("opening one tree produces one session");
    attach(context, &session.name)
}

pub(crate) fn should_attach(context: &Context, no_attach: bool) -> bool {
    attachment_allowed(
        context.settings.session.backend,
        context.settings.session.attach,
        context.tty,
        context.json,
        context.parent_env.contains_key("WT_ACTIVATION"),
        no_attach,
    )
}

fn attachment_allowed(
    backend: SessionBackend,
    attach: bool,
    tty: wt_sys::snapshot::Tty,
    json: bool,
    inside_door: bool,
    no_attach: bool,
) -> bool {
    backend == SessionBackend::Tmux
        && attach
        && tty.stdin
        && tty.stdout
        && !json
        && !inside_door
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
    contain_failures: bool,
) -> Result<OpenBatch, CoreError> {
    let tmux = tmux(context);
    let mut sessions = Vec::new();
    let mut notices = Vec::new();
    let mut worst = None;
    for tree in trees {
        let target = super::context::target_of(&tree);
        let result = open_tree(context, &tmux, tree.clone(), agent_override);
        let (session, tree_notices) = match result {
            Ok(result) => result,
            Err(error) if contain_failures => {
                let notice = Notice {
                    level: NoticeLevel::Warn,
                    code: error.code.0.clone(),
                    subject: Some(target.to_string()),
                    message: format!("{}; remedy: {}", error.message, error.remedy),
                    guidance: None,
                };
                notices.push(notice);
                if worst
                    .as_ref()
                    .is_none_or(|current: &CoreError| error.exit() > current.exit())
                {
                    worst = Some(error.clone());
                }
                sessions.push(SessionReport::Failed(FailedSessionReport {
                    target: target.to_string(),
                    name: tree.session_name,
                    failed: true,
                    code: error.code.0,
                    message: error.message,
                    remedy: error.remedy,
                }));
                continue;
            }
            Err(error) => return Err(error),
        };
        notices.extend(tree_notices);
        sessions.push(SessionReport::Open(session));
    }
    Ok(OpenBatch {
        sessions,
        notices,
        failure: worst,
    })
}

struct OpenBatch {
    sessions: Vec<SessionReport>,
    notices: Vec<Notice>,
    failure: Option<CoreError>,
}

fn open_tree(
    context: &mut Context,
    tmux: &wt_sys::tmux::Tmux,
    tree: TreeRec,
    agent_override: Option<&str>,
) -> Result<(OpenSessionReport, Vec<Notice>), CoreError> {
    let target = super::context::target_of(&tree);
    let mut gate = door::enter(context, Some(&target.to_string()), "open", false)?;
    let notices = gate.notices.clone();
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
    Ok((
        OpenSessionReport {
            target: target.to_string(),
            name: tree.session_name,
            created,
            existing,
            agent: reported_agent,
            foreground: false,
        },
        notices,
    ))
}

pub(crate) fn session_failure_notice(target: &str, error: &CoreError) -> Notice {
    Notice {
        level: NoticeLevel::Warn,
        code: "SESSION_CREATE_FAILED".to_owned(),
        subject: Some(target.to_owned()),
        message: format!(
            "session for {target} was not created: {}; run `wt open {target}` to retry",
            error.message
        ),
        guidance: None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_requires_a_terminal_on_input_and_output() {
        let terminal = wt_sys::snapshot::Tty {
            stdin: true,
            stdout: true,
            stderr: true,
        };
        assert!(attachment_allowed(
            SessionBackend::Tmux,
            true,
            terminal,
            false,
            false,
            false
        ));
        assert!(!attachment_allowed(
            SessionBackend::Tmux,
            true,
            wt_sys::snapshot::Tty {
                stdin: false,
                ..terminal
            },
            false,
            false,
            false
        ));
        assert!(!attachment_allowed(
            SessionBackend::Tmux,
            true,
            wt_sys::snapshot::Tty {
                stdout: false,
                ..terminal
            },
            false,
            false,
            false
        ));
    }
}
