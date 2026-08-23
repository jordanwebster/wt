use wt_core::lifecycle::StatePhase;
use wt_core::model::{AbsPath, Label, SourceKind, Target, TreeRec, TreeSource};
use wt_core::report::AdoptData;
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, Mode};

use crate::cli::Adopt;

use super::{door, executor, list, register, Context, Output};

pub(crate) fn run(context: &mut Context, args: Adopt) -> Result<Output, CoreError> {
    let path = wt_sys::fsx::canonicalize(&args.path)?;
    let git = context.git(&path)?;
    let common = wt_sys::fsx::canonicalize(&git.common_dir()?)?;
    let gitdir = wt_core::model::gitdir_id(&common.to_string_lossy());
    let registered_label = context
        .registry
        .labels
        .iter()
        .find(|(_, record)| record.gitdir_id == gitdir)
        .map(|(label, _)| label.clone());
    let label = match (args.label, registered_label) {
        (Some(requested), Some(actual)) if requested != actual.as_str() => {
            return Err(CoreError::new(
                ExitClass::Conflict,
                "GITDIR_REGISTERED",
                format!("worktree belongs to label {actual}"),
                format!("use `--label {actual}`"),
            ))
        }
        (Some(requested), _) => Label::new(requested)?,
        (None, Some(actual)) => actual,
        (None, None) => {
            return Err(CoreError::new(
                ExitClass::Conflict,
                "GITDIR_REGISTERED",
                "worktree's repository is not registered",
                "register its canonical checkout first",
            ))
        }
    };
    let listed = git.worktrees()?.iter().any(|worktree| {
        wt_sys::fsx::canonicalize(&worktree.path).is_ok_and(|listed| listed == path)
    });
    if !listed {
        return Err(CoreError::new(
            ExitClass::State,
            "NOT_A_WORKTREE",
            "path is not listed by git worktree list",
            "adopt a linked worktree belonging to the registered repository",
        ));
    }
    let name = args.name.unwrap_or_else(|| {
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("work")
            .to_owned()
    });
    let target = Target {
        label: label.clone(),
        name: wt_core::model::TreeName::new(name)?.to_string(),
    };
    if let Some(existing) = context
        .registry
        .trees
        .iter()
        .find(|tree| tree.label == target.label && tree.name == target.name)
        .cloned()
    {
        let interrupted = context
            .read_state(&target)?
            .is_some_and(|state| state.phase == StatePhase::Initialising);
        let (door, resumed) = if interrupted {
            let holder = context.holder(target.to_string(), "adopt")?;
            let _token = lock::tree(
                &context.tree_lock_path(&target),
                Mode::Exclusive,
                &holder,
                context.tree_wait(None),
            )?;
            let door = door::enter_held(context, existing.clone(), "adopt", false, true)?;
            executor::refresh_all_declarations(context, &door)?;
            context.mutate_state(&target, &holder, |state| {
                state.phase = StatePhase::Ready;
                state.op = None;
                Ok(())
            })?;
            (door, true)
        } else {
            let door = door::enter(context, Some(&target.to_string()), "adopt", false)?;
            executor::refresh_all_declarations(context, &door)?;
            (door, false)
        };
        return Ok(Output::data(AdoptData {
            tree: list::tree_report(context, &existing, false, false, false)?,
            adopted: false,
            resumed,
        })?
        .with_notices(door.notices));
    }
    if context
        .registry
        .trees
        .iter()
        .any(|tree| tree.path.as_str() == path.to_string_lossy())
    {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "PATH_REGISTERED",
            "path is already registered",
            "use the existing target",
        ));
    }
    let holder = context.holder(target.to_string(), "adopt")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    let config = register::initial_config(context, &path, &label)?;
    let coordinates = context.allocate(&target, &config.ports)?;
    let now = wt_sys::fsx::timestamp()?;
    let tree_id = wt_sys::fsx::random_tree_id()?;
    let state = register::new_state(&target, &tree_id, StatePhase::Initialising, "adopt", &now)?;
    context.write_state(&target, &state, &holder)?;
    let tree = TreeRec {
        tree_id: tree_id.clone(),
        label,
        name: target.name.clone(),
        canonical: false,
        path: AbsPath::new(path.to_string_lossy().into_owned())?,
        slot: coordinates.slot,
        geometry: coordinates.geometry,
        ports: coordinates.ports,
        name_short: coordinates.name_short,
        session_name: coordinates.session_name,
        created_at: now,
        agent: None,
        source: TreeSource {
            kind: SourceKind::Adopted,
            branch: git.head_branch(&path)?,
            pr: None,
            start: None,
        },
    };
    context.mutate_registry(&holder, |registry| {
        registry.trees.push(tree.clone());
        Ok(())
    })?;
    wt_sys::fsx::write_nofollow(
        &path,
        &wt_core::model::RelPath::new(".wt/tree_id")?,
        format!("{tree_id}\n").as_bytes(),
        0o600,
    )?;
    door::recompute_exclude(context, &tree.label)?;
    let door = door::enter_held(context, tree.clone(), "adopt", false, true)?;
    executor::refresh_all_declarations(context, &door)?;
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Ready;
        state.op = None;
        Ok(())
    })?;
    drop(token);
    context.reload_registry()?;
    Ok(Output::data(AdoptData {
        tree: list::tree_report(context, &tree, false, false, false)?,
        adopted: true,
        resumed: false,
    })?
    .with_notices(door.notices))
}
