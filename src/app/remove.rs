use std::path::Path;
use std::time::Duration;

use wt_core::lifecycle::{OpVerb, Operation, StatePhase};
use wt_core::model::Tombstone;
use wt_core::remove::{AfterDestroy, GitObs, Obs, RemoveOptions, Upstream};
use wt_core::report::{DestroyedReport, RemoveData};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, Mode};

use crate::cli::Remove;

use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: Remove) -> Result<Output, CoreError> {
    let target = wt_core::model::Target::parse(&args.target)?;
    let Some(tree) = context
        .registry
        .trees
        .iter()
        .find(|tree| tree.label == target.label && tree.name == target.name)
        .cloned()
    else {
        return Output::data(RemoveData {
            target: target.to_string(),
            removed: false,
            destroyed: Vec::new(),
            orphans_kept: Vec::new(),
            branch_deleted: false,
            session_closed: false,
        });
    };
    let state = context.read_state(&target)?;
    let dir_exists = matches!(
        wt_sys::fsx::path_kind(Path::new(tree.path.as_str()))?,
        wt_sys::fsx::PathKind::Directory
    );
    let identity_ok = dir_exists && context.identity_ok(&tree)?;
    let git_obs = observe_git(context, &tree, dir_exists && identity_ok)?;
    let session_live = tmux_has(context, &tree).unwrap_or(false);
    let resources = state
        .as_ref()
        .map(|state| state.resources.keys().cloned().collect())
        .unwrap_or_default();
    let observed = Obs {
        target: target.to_string(),
        canonical: tree.canonical,
        dir_exists,
        identity_ok,
        git: git_obs,
        session_live,
        door_holders: door_holders(context, &target)?,
        resources,
    };
    let plan = wt_core::remove::plan_with_options(
        &observed,
        RemoveOptions {
            force: args.force,
            keep_orphans: args.keep_orphans,
        },
    )?;
    let prompt = format!(
        "remove plan: {}",
        serde_json::to_string(&plan).unwrap_or_else(|_| target.to_string())
    );
    if !context.confirm(&prompt)? {
        return Output::data(RemoveData {
            target: target.to_string(),
            removed: false,
            destroyed: Vec::new(),
            orphans_kept: Vec::new(),
            branch_deleted: false,
            session_closed: false,
        });
    }

    let session_closed = if session_live {
        super::close::close_tree(context, &tree)?
    } else {
        false
    };
    let holder = context.holder(target.to_string(), "remove")?;
    let _tree_lock = match lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(args.wait.as_deref()),
    ) {
        Ok(token) => token,
        Err(mut error) if error.code.0 == "TREE_IN_USE" => {
            let holders = door_holders(context, &target)?;
            if !holders.is_empty() {
                error.message = format!(
                    "tree is in use by {}",
                    holders
                        .iter()
                        .map(|holder| format!("pid {} ({})", holder.pid, holder.verb))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            return Err(error.with_details(serde_json::json!({
                "holders": holders,
            })));
        }
        Err(error) => return Err(error),
    };
    let current_dir_exists = matches!(
        wt_sys::fsx::path_kind(Path::new(tree.path.as_str()))?,
        wt_sys::fsx::PathKind::Directory
    );
    let current_identity_ok = current_dir_exists && context.identity_ok(&tree)?;
    let reobserved = Obs {
        target: target.to_string(),
        canonical: tree.canonical,
        dir_exists: current_dir_exists,
        identity_ok: current_identity_ok,
        git: observe_git(context, &tree, current_dir_exists && current_identity_ok)?,
        session_live: false,
        door_holders: door_holders(context, &target)?,
        resources: context
            .read_state(&target)?
            .map(|state| state.resources.keys().cloned().collect())
            .unwrap_or_default(),
    };
    wt_core::remove::revalidate(&plan, &reobserved, args.force)?;
    let now = wt_sys::fsx::timestamp()?;
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Removing;
        state.op = Some(Operation {
            verb: OpVerb::Remove,
            pid: std::process::id(),
            started: now.clone(),
        });
        Ok(())
    })?;
    let entered = if current_dir_exists && current_identity_ok {
        let door = door::enter_held(context, tree.clone(), "remove", false, false)?;
        executor::refresh_all_declarations(context, &door)?;
        Some(door)
    } else {
        None
    };

    let mut destroyed = Vec::new();
    let mut errors = Vec::new();
    let records = context
        .read_state(&target)?
        .map(|state| state.resources.into_values().collect::<Vec<_>>())
        .unwrap_or_default();
    for record in records {
        let key = record.key.clone();
        let resource_holder = context.holder(target.to_string(), "remove")?;
        let _resource = lock::resource(
            &resource_lock_path(context, &key),
            &resource_holder,
            super::context::duration(
                context.settings.locks.resource.as_deref(),
                Duration::from_secs(120),
            ),
        )?;
        let result = if let Some(door) = entered.as_ref() {
            executor::destroy_resource(context, door, &key, true)
        } else {
            executor::destroy_stored_resource(context, &target, record, !identity_ok)
        };
        match result {
            Ok((status, child)) => destroyed.push(DestroyedReport {
                scope: key.scope.to_string(),
                task: key.task,
                state: status,
                child,
            }),
            Err(error) => {
                errors.push(error);
                destroyed.push(DestroyedReport {
                    scope: key.scope.to_string(),
                    task: key.task,
                    state: "orphaned".to_owned(),
                    child: None,
                });
            }
        }
    }
    wt_sys::failpoint::remove_8()?;
    let state = context.read_state(&target)?.ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "STATE_CORRUPT",
            "tree state disappeared during removal",
            "run `wt doctor`",
        )
    })?;
    let records_remaining = !state.resources.is_empty();
    let after = wt_core::remove::after_destroy(&plan, records_remaining)?;
    if !errors.is_empty() && after != AfterDestroy::KeepLiveEntry {
        return Err(errors.remove(0));
    }
    let git = context.git(Path::new(
        context.registry.labels[&tree.label].path.as_str(),
    ))?;
    let git_holder = context.holder(target.to_string(), "remove")?;
    let _git_lock = lock::git(
        &context.git_lock_path(&context.registry.labels[&tree.label].gitdir_id),
        &git_holder,
        super::context::duration(
            context.settings.locks.repo_git.as_deref(),
            Duration::from_secs(60),
        ),
    )?;
    if dir_exists && identity_ok {
        git.worktree_remove(Path::new(tree.path.as_str()), true)?;
    } else if !identity_ok && dir_exists {
        // A replacement directory is never passed to git or removed.
    } else {
        git.worktree_prune()?;
    }
    let mut branch_deleted = false;
    if args.delete_branch {
        if let Some(branch) = &tree.source.branch {
            git.branch_delete(branch, args.force)?;
            branch_deleted = true;
        }
    }
    let orphans_kept = if after == AfterDestroy::KeepLiveEntry {
        context.mutate_state(&target, &holder, |state| {
            state.op = None;
            Ok(())
        })?;
        state.resources.keys().cloned().collect()
    } else {
        let materialized = state
            .materialized
            .iter()
            .filter_map(|item| wt_core::model::RelPath::new(&item.path).ok())
            .collect();
        let tombstone = Tombstone {
            label: tree.label.clone(),
            name: tree.name.clone(),
            slot: tree.slot,
            geometry: tree.geometry,
            ports: tree.ports.clone(),
            name_short: tree.name_short.clone(),
            session_name: tree.session_name.clone(),
            path: tree.path.clone(),
            materialized,
            removed_at: wt_sys::fsx::timestamp()?,
            reason: "removed".to_owned(),
        };
        context.mutate_registry(&holder, |registry| {
            registry
                .trees
                .retain(|record| record.tree_id != tree.tree_id);
            registry.tombstones.push(tombstone);
            Ok(())
        })?;
        wt_sys::fsx::remove_path(&context.state_path(&target))?;
        door::recompute_exclude(context, &tree.label)?;
        Vec::new()
    };
    Output::data(RemoveData {
        target: target.to_string(),
        removed: true,
        destroyed,
        orphans_kept,
        branch_deleted,
        session_closed,
    })
}

pub(crate) fn door_holders(
    context: &Context,
    target: &wt_core::model::Target,
) -> Result<Vec<wt_core::remove::DoorHolder>, CoreError> {
    let directory = context
        .home
        .join(format!("locks/{}/{}.doors", target.label, target.name));
    let mut holders = Vec::new();
    for path in wt_sys::fsx::read_dir_paths(&directory)? {
        if !lock::is_held(&path)? {
            continue;
        }
        if let Some(holder) = lock::read_holder(&path)? {
            holders.push(wt_core::remove::DoorHolder {
                pid: holder.pid,
                verb: holder.verb,
                since: holder.since,
            });
        }
    }
    holders.sort_by(|left, right| {
        (left.pid, &left.verb, &left.since).cmp(&(right.pid, &right.verb, &right.since))
    });
    Ok(holders)
}

pub(crate) fn observe_git(
    context: &Context,
    tree: &wt_core::model::TreeRec,
    owned_directory: bool,
) -> Result<Option<GitObs>, CoreError> {
    if !owned_directory {
        return Ok(None);
    }
    let root = Path::new(tree.path.as_str());
    let git = context.git(root)?;
    let entries = git.status_porcelain(root)?;
    let branch = git.head_branch(root)?;
    let (upstream, ahead) = if let Some(branch) = branch.as_deref() {
        let track = git.upstream_track(branch)?;
        if track == "[gone]" {
            (Upstream::Gone, 0)
        } else if let Some(upstream) = git.upstream(branch)? {
            (
                Upstream::Present,
                u32::try_from(git.ahead_behind("HEAD", &upstream)?.ahead).unwrap_or(u32::MAX),
            )
        } else {
            (Upstream::None, 0)
        }
    } else {
        (Upstream::None, 0)
    };
    let remote_contains_head = !git.remote_branches_containing("HEAD")?.is_empty();
    let default = git.default_branch()?;
    let default_ref = format!("refs/remotes/origin/{default}");
    let merged = if git.ref_exists(&default_ref)? {
        git.is_ancestor("HEAD", &default_ref)?
            && git.resolve_commit("HEAD")? != git.resolve_commit(&default_ref)?
    } else {
        false
    };
    Ok(Some(GitObs {
        dirty_porcelain: entries
            .iter()
            .map(|entry| format!("{}{} {}", entry.index, entry.worktree, entry.path.display()))
            .collect::<Vec<_>>()
            .join("\n"),
        upstream,
        ahead,
        remote_contains_head,
        detached: branch.is_none(),
        merged,
    }))
}

fn tmux_has(context: &Context, tree: &wt_core::model::TreeRec) -> Result<bool, CoreError> {
    let timeout = wt_core::model::duration_millis(&context.settings.session.tmux_timeout)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(10));
    let tmux = wt_sys::tmux::Tmux::new("tmux", timeout);
    match tmux.has_session(&tree.session_name) {
        Err(error) if error.code.0 == "TOOL_MISSING" => Ok(false),
        result => result,
    }
}

fn resource_lock_path(
    context: &Context,
    key: &wt_core::resource::ResourceKey,
) -> std::path::PathBuf {
    let tied = match key.tied_to {
        wt_core::config::TiedTo::Tree => {
            format!("tree/{}/", key.name.as_deref().unwrap_or("canonical"))
        }
        wt_core::config::TiedTo::Repo => "repo/".to_owned(),
    };
    context.home.join(format!(
        "locks/{}/res/{tied}{}/{}.lock",
        key.label,
        wt_core::model::scope_enc(&key.scope),
        key.task
    ))
}
