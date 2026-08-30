use std::collections::BTreeMap;

use wt_core::config::LockCfg;
use wt_core::report::{HolderReport, LockReport, LocksData, SlotHolderReport};
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
            held_slots: None,
            slots: None,
            holders: Vec::new(),
        });
    }

    let mut named = BTreeMap::<(String, String), LockCfg>::new();
    let mut trees = context
        .registry
        .trees
        .iter()
        .filter(|tree| {
            args.label
                .as_deref()
                .is_none_or(|label| tree.label.as_str() == label)
        })
        .collect::<Vec<_>>();
    trees.sort_by_key(|tree| !tree.canonical);
    for tree in trees {
        for (name, lock) in context.load_config(tree)?.locks {
            named.entry((tree.label.to_string(), name)).or_insert(lock);
        }
    }
    for ((label, name), config) in named {
        let path = context.home.join(format!("locks/{label}/named/{name}"));
        let occupancy = wt_sys::lock::named_occupancy(&path, config.slots)?;
        let held_slots = u16::try_from(occupancy.holders.len()).unwrap_or(u16::MAX);
        let holders = occupancy
            .holders
            .into_iter()
            .map(|slot| SlotHolderReport {
                slot: slot.slot,
                path: slot.path.to_string_lossy().into_owned(),
                holder: slot.holder.map(holder_report),
            })
            .collect();
        locks.push(LockReport {
            level: 4,
            name: format!("{label}:{name}"),
            path: path.to_string_lossy().into_owned(),
            held: held_slots > 0,
            holder: None,
            held_slots: Some(held_slots),
            slots: Some(config.slots),
            holders,
        });
    }
    locks.sort_by(|left, right| (left.level, &left.name).cmp(&(right.level, &right.name)));
    Output::data(LocksData { locks })
}

fn holder_report(holder: wt_sys::lock::Holder) -> HolderReport {
    HolderReport {
        pid: holder.pid,
        target: holder.target,
        verb: holder.verb,
        since: holder.since,
    }
}
