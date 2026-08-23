use wt_core::report::{HolderReport, LockReport, LocksData};
use wt_core::CoreError;

use crate::cli::Locks;

use super::{Context, Output};

pub(crate) fn run(context: &mut Context, args: Locks) -> Result<Output, CoreError> {
    let mut locks = Vec::new();
    for tree in &context.registry.trees {
        if args
            .label
            .as_deref()
            .is_some_and(|label| tree.label.as_str() != label)
        {
            continue;
        }
        let target = super::context::target_of(tree);
        let path = context.tree_lock_path(&target);
        let holder = wt_sys::lock::read_holder(&path)?.map(|holder| HolderReport {
            pid: holder.pid,
            target: holder.target,
            verb: holder.verb,
            since: holder.since,
        });
        locks.push(LockReport {
            level: 1,
            name: target.to_string(),
            path: path.to_string_lossy().into_owned(),
            held: wt_sys::lock::is_held(&path)?,
            holder,
        });
    }
    locks.sort_by(|left, right| (left.level, &left.name).cmp(&(right.level, &right.name)));
    Output::data(LocksData { locks })
}
