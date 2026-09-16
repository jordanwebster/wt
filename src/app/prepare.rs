use wt_core::lifecycle::{OpVerb, Operation, StatePhase};
use wt_core::report::PrepareData;
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, Mode};

use crate::cli::{Adopt, Prepare};

use super::{adopt, list, new, AfterRender, Context, Output};

pub(crate) fn run(context: &mut Context, args: Prepare) -> Result<Output, CoreError> {
    let no_sync = args.no_sync;
    let no_after_new = args.no_after_new;
    let adoption = adopt::record(
        context,
        Adopt {
            path: args.path,
            label: args.label,
            name: args.name,
            agent: args.agent,
            meta: args.meta,
        },
        true,
    )?;
    let tree = adoption.tree;
    let target = super::context::target_of(&tree);
    let canonical = context
        .registry
        .trees
        .iter()
        .find(|candidate| candidate.label == tree.label && candidate.canonical)
        .cloned()
        .ok_or_else(|| {
            CoreError::new(
                ExitClass::NotFound,
                "NOT_FOUND",
                format!("canonical checkout for {} is not registered", tree.label),
                "register the repository's canonical checkout first",
            )
        })?;
    let holder = context.holder(target.to_string(), "prepare")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    context.require_identity(&tree)?;
    let now = wt_sys::fsx::timestamp()?;
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Bootstrapping;
        state.op = Some(Operation {
            verb: OpVerb::Prepare,
            pid: std::process::id(),
            started: now.clone(),
        });
        Ok(())
    })?;
    let config = context.load_config(&tree)?;
    let finished = new::finish_under_lock(
        context,
        &tree,
        &canonical,
        &config,
        &holder,
        new::FinishOptions {
            no_sync,
            start: wt_core::new::StartAt::Git,
            verb: "prepare",
        },
    )?;
    drop(token);
    context.reload_registry()?;
    let mut output = Output::data(PrepareData {
        tree: list::tree_report(context, &tree, false, false, false)?,
        adopted: adoption.adopted,
        resumed: adoption.resumed,
        sync: finished.sync,
        after_new: None,
    })?
    .with_notices(adoption.notices.into_iter().chain(finished.notices));
    let address = target.to_string();
    if !no_after_new && new::has_after_new_hook(context, &address)? {
        output = output.after_render(AfterRender::AfterNew { target: address });
    }
    Ok(output)
}
