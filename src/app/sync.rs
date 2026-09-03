use std::path::{Path, PathBuf};

use wt_core::lifecycle::{OpVerb, Operation, StatePhase, SyncState};
use wt_core::report::{SyncData, SyncInputReport};
use wt_core::CoreError;
use wt_sys::lock::{self, Mode};

use crate::cli::Sync;

use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: Sync) -> Result<Output, CoreError> {
    let target = context.resolve(args.target.as_deref())?;
    let tree = context.tree(&target)?;
    let holder = context.holder(target.to_string(), "sync")?;
    let _tree = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    context.require_identity(&tree)?;
    let prior = context.read_state(&target)?.ok_or_else(|| {
        wt_core::CoreError::new(
            wt_core::ExitClass::State,
            "STATE_CORRUPT",
            "tree state is missing",
            "run `wt doctor` and repair the state",
        )
    })?;
    let door = match door::enter_held(context, tree.clone(), "sync", true) {
        Ok(door) => door,
        Err(error) => {
            fail(context, &target, &holder, &error)?;
            return Err(error);
        }
    };
    let plan = match executor::plan(context, &door, "sync") {
        Ok(plan) => plan,
        Err(error) => {
            if !matches!(prior.phase, StatePhase::Ready | StatePhase::Failed) {
                context.mutate_state(&target, &holder, |state| {
                    state.phase = StatePhase::Failed;
                    state.op = None;
                    state.last_error = Some(error.to_string());
                    Ok(())
                })?;
            }
            return Err(error);
        }
    };
    let now = wt_sys::fsx::timestamp()?;
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Bootstrapping;
        state.op = Some(Operation {
            verb: OpVerb::Sync,
            pid: std::process::id(),
            started: now.clone(),
        });
        Ok(())
    })?;
    if let Err(error) = executor::refresh_all_declarations(context, &door) {
        fail(context, &target, &holder, &error)?;
        return Err(error);
    }
    let paths = door
        .config
        .sync_inputs
        .iter()
        .map(|path| PathBuf::from(path.as_str()))
        .collect::<Vec<_>>();
    let hashes = context
        .git(Path::new(tree.path.as_str()))?
        .hash_object(&paths)?;
    let inputs = paths
        .iter()
        .zip(hashes)
        .map(|(path, hash)| SyncInputReport {
            path: path.to_string_lossy().into_owned(),
            hash,
        })
        .collect::<Vec<_>>();
    let old = context.read_state(&target)?.and_then(|state| state.sync);
    let unchanged = old.as_ref().is_some_and(|sync| {
        sync.ok
            && sync.inputs
                == inputs
                    .iter()
                    .map(|input| (input.path.clone(), input.hash.clone()))
                    .collect()
    });
    let (ran, steps) = if unchanged && !args.force {
        (false, Vec::new())
    } else {
        match executor::execute_plan(
            context,
            &door,
            &plan,
            None,
            executor::ExecuteOptions::DEFAULT,
        ) {
            Ok(result) => (true, result.data.steps),
            Err(error) => {
                fail(context, &target, &holder, &error)?;
                return Err(error);
            }
        }
    };
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Ready;
        state.op = None;
        state.sync = Some(SyncState {
            at: now,
            ok: true,
            inputs: inputs
                .iter()
                .map(|input| (input.path.clone(), input.hash.clone()))
                .collect(),
            log: None,
        });
        state.last_error = None;
        Ok(())
    })?;
    let mut notices = door.notices;
    // A linked tree that just caught up with its inputs is the moment the
    // canonical is likely behind too (§11.10).
    notices.extend(super::anchor::spawn_after(context, &tree, true));
    Ok(Output::data(SyncData {
        target: target.to_string(),
        ran,
        steps,
        inputs,
    })?
    .with_notices(notices))
}

fn fail(
    context: &Context,
    target: &wt_core::model::Target,
    holder: &wt_sys::lock::Holder,
    error: &CoreError,
) -> Result<(), CoreError> {
    context.mutate_state(target, holder, |state| {
        state.phase = StatePhase::Failed;
        state.op = None;
        state.last_error = Some(error.to_string());
        if let Some(sync) = state.sync.as_mut() {
            sync.ok = false;
        }
        Ok(())
    })
}
