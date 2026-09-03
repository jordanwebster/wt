//! The build verb's bookkeeping (SPEC §11.2 automatic build, §11.9): the
//! status file and recorded pid that `status`, `doctor` and the shims read,
//! the detached supervisor that runs a build after `new` returns, and the
//! sweep that follows every build wt launches.

use std::path::{Path, PathBuf};

use wt_core::lifecycle::BuildState;
use wt_core::model::Target;
use wt_core::{CoreError, ExitClass};

use crate::cli::AliasRun;

use super::{run, sweep, Context, Output};

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

/// `wt build`: the alias run, with status bookkeeping when the tree has a
/// recorded build slot or a supervisor named one through the environment.
pub(crate) fn dispatch(context: &mut Context, args: AliasRun) -> Result<Output, CoreError> {
    let slot = if args.dry_run {
        None
    } else {
        slot_for(context, args.target.as_deref())
    };
    run_build(context, args, slot).map(|(output, _)| output)
}

/// Runs the effective `build` task as `wt build` does — status `running`
/// then `ok`/`failed`, this process as the recorded owner — and sweeps the
/// tree afterwards. Without a slot the run is a plain task run.
pub(crate) fn run_build(
    context: &mut Context,
    args: AliasRun,
    slot: Option<Slot>,
) -> Result<(Output, Option<wt_core::report::SweepReport>), CoreError> {
    let target = args.target.clone();
    if let Some(slot) = &slot {
        wt_sys::fsx::write_store(&slot.status, b"running\n")?;
        // The recorded pid otherwise still names the finished supervisor,
        // and a dead pid beside a `running` status reads as abandoned (A69)
        // — for the whole foreground run.
        record_owner(context, target.as_deref(), slot);
        // The build's log is the slot's, so the record and the tee agree.
        context.parent_env.insert(
            "WT_BUILD_LOG".to_owned(),
            slot.log.to_string_lossy().into_owned(),
        );
    }
    let dry_run = args.dry_run;
    let result = run::run(context, args.into_run("build"));
    if let Some(slot) = &slot {
        let status = if result.is_ok() { "ok\n" } else { "failed\n" };
        let _ = wt_sys::fsx::write_store(&slot.status, status.as_bytes());
    }
    if dry_run {
        return result.map(|output| (output, None));
    }
    // A failed build leaves superseded units behind as readily as a
    // finished one; the sweep runs either way, and on failure its notices
    // ride on the error envelope through the pending set.
    let (notices, swept) = sweep::after_build(context, target.as_deref());
    match result {
        Ok(output) => Ok((output.with_notices(notices), swept)),
        Err(error) => {
            context.pending_notices.extend(notices);
            Err(error)
        }
    }
}

/// The slot a plain `wt build` writes: the one named by a supervisor's
/// environment, else the tree's own when a build was recorded before.
fn slot_for(context: &Context, target: Option<&str>) -> Option<Slot> {
    let target = context.resolve(target).ok()?;
    let tree = context.tree(&target).ok()?;
    let slot = Slot::for_tree(&tree);
    if let Some(path) = std::env::var_os("WT_BACKGROUND_BUILD_STATUS").map(PathBuf::from) {
        return (path == slot.status).then_some(slot);
    }
    context.read_state(&target).ok()??.build.as_ref()?;
    Some(slot)
}

/// Records this process as the build's owner, at the tree's current commit.
fn record_owner(context: &Context, target: Option<&str>, slot: &Slot) {
    let Ok(target) = context.resolve(target) else {
        return;
    };
    let Ok(holder) = context.holder(target.to_string(), "build") else {
        return;
    };
    let head = head_of(context, &target);
    let _ = context.mutate_state(&target, &holder, |state| {
        let build = state.build.get_or_insert_with(|| BuildState {
            started: String::new(),
            log: slot.log.to_string_lossy().into_owned(),
            pid: 0,
            head: None,
        });
        build.pid = std::process::id();
        build.started = wt_sys::fsx::timestamp()?;
        build.head = head.clone();
        Ok(())
    });
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
