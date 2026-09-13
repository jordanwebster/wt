use std::path::Path;

use wt_core::CoreError;

use crate::cli::HookRun;

use super::{build, door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: HookRun) -> Result<Output, CoreError> {
    let mut door = door::enter(context, Some(&args.target), "hook")?;
    if args.kind == "idle" {
        super::with_resources::destroy_idle_declared(context, &door)?;
        return Output::data(serde_json::json!({
            "target": args.target,
            "cleanup": "idle",
        }));
    }
    let command = match args.kind.as_str() {
        "after-new" => door.config.hooks.after_new.clone(),
        "warm" => door.config.hooks.warm.clone(),
        _ => None,
    }
    .ok_or_else(|| {
        CoreError::new(
            wt_core::ExitClass::State,
            "HOOK_MISSING",
            format!("hook `{}` is not declared", args.kind),
            "declare the hook and retry",
        )
    })?;
    let slot = build::Slot::for_tree(&door.tree);
    wt_sys::fsx::create_private_dir(slot.log.parent().expect("hook log has a directory"))?;
    wt_sys::fsx::write_store(&slot.status, b"running\n")?;
    door.release_gate();
    let result = executor::run_hook(context, &door, &args.kind, &command, &slot.log);
    let status: &[u8] = if result.is_ok() { b"ok\n" } else { b"failed\n" };
    let _ = wt_sys::fsx::write_store(&slot.status, status);
    result?;
    Output::data(serde_json::json!({
        "target": args.target,
        "hook": args.kind,
        "log": Path::new(&slot.log),
    }))
}
