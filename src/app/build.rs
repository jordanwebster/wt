//! Hook bookkeeping shared by `after_new`, `warm`, status, and command shims.

use std::path::{Path, PathBuf};

use wt_core::model::Target;
use wt_core::{CoreError, ExitClass};

use super::Context;

/// Where one build's status lives and which log it writes: one slot owned
/// by the most recent build starter (A69).
pub(crate) struct Slot {
    pub status: PathBuf,
    pub log: PathBuf,
}

impl Slot {
    pub fn for_tree(tree: &wt_core::model::TreeRec) -> Self {
        let root = Path::new(tree.path.as_str());
        Self {
            status: root.join(".wt/build.status"),
            log: root.join(".wt/logs/wt-setup.log"),
        }
    }
}

pub(crate) fn head_of(context: &Context, target: &Target) -> Option<String> {
    let tree = context.tree(target).ok()?;
    let path = Path::new(tree.path.as_str());
    context.git(path).ok()?.head_oid_in(path).ok()
}

/// Whether the recorded build covers the tree's current commit: status
/// `ok`, and the commit it ran at is the one HEAD names now.
pub(crate) fn is_fresh(
    context: &Context,
    tree: &wt_core::model::TreeRec,
    head: &str,
) -> Result<bool, CoreError> {
    let target = super::context::target_of(tree);
    let Some(build) = context.read_state(&target)?.and_then(|state| state.build) else {
        return Ok(false);
    };
    if build.head.as_deref() != Some(head) {
        return Ok(false);
    }
    let status = wt_sys::fsx::read_string(&Slot::for_tree(tree).status)?;
    Ok(status.as_deref().map(str::trim) == Some("ok"))
}

/// Launches the running wt binary detached — setsid, double fork, reparented
/// — with the given arguments, so it outlives this CLI (§11.2 automatic
/// build). Returns the supervisor's pid.
pub(crate) fn spawn_wt(
    context: &Context,
    cwd: &Path,
    args: &[&str],
    env: &[(&str, String)],
    log: Option<&Path>,
    nice: Option<i32>,
) -> Result<u32, CoreError> {
    let running_binary = std::env::current_exe().map_err(|error| {
        CoreError::new(
            ExitClass::Internal,
            "CURRENT_EXE_FAILED",
            format!("could not resolve the running wt binary: {error}"),
            "retry and report this wt bug if it repeats",
        )
    })?;
    let mut request = wt_sys::proc::CommandRequest::new(running_binary);
    request.args = wt_sys::proc::os_args(args);
    request.cwd = Some(cwd.to_path_buf());
    request.env.insert(
        "WT_HOME".to_owned(),
        context.home.to_string_lossy().into_owned(),
    );
    for (key, value) in env {
        request.env.insert((*key).to_owned(), value.clone());
    }
    request.nice = nice;
    wt_sys::proc::spawn_detached_logged(&request, log)
}
