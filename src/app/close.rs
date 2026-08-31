use std::time::Duration;

use wt_core::report::{ClosedSessionReport, Notice, NoticeLevel, SessionReport, SessionsData};
use wt_core::settings::SessionBackend;
use wt_core::CoreError;

use crate::cli::Close;

use super::{Context, Output};

pub(crate) fn run(context: &mut Context, args: Close) -> Result<Output, CoreError> {
    let backend_notice = super::register::resolve_session_backend(context)?;
    let trees = if args.all {
        context.registry.trees.clone()
    } else {
        let target = context.resolve(args.target.as_deref())?;
        vec![context.tree(&target)?]
    };
    let mut sessions = Vec::new();
    for tree in trees {
        let closed = if context.settings.session.backend == SessionBackend::None {
            false
        } else {
            close_tree(context, &tree)?
        };
        sessions.push(SessionReport::Closed(ClosedSessionReport {
            target: super::context::target_of(&tree).to_string(),
            session: tree.session_name,
            closed,
        }));
    }
    let mut output = Output::data(SessionsData { sessions })?;
    if let Some(notice) = backend_notice {
        output = output.with_notices([notice]);
    }
    if context.settings.session.backend == SessionBackend::None {
        output = output.with_notices([Notice {
            level: NoticeLevel::Info,
            code: "SESSIONS_DISABLED".to_owned(),
            subject: None,
            message: "sessions are disabled; nothing was closed".to_owned(),
        }]);
    }
    Ok(output)
}

pub(crate) fn close_tree(
    context: &Context,
    tree: &wt_core::model::TreeRec,
) -> Result<bool, CoreError> {
    let timeout = wt_core::model::duration_millis(&context.settings.session.tmux_timeout)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(10));
    let tmux = wt_sys::tmux::Tmux::new("tmux", timeout);
    let exists = tmux.has_session(&tree.session_name)?;
    if exists {
        tmux.kill_session(&tree.session_name)?;
    }
    Ok(exists)
}
