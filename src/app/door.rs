use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use wt_core::config::{self, Config};
use wt_core::env::{EnvInputs, EnvOutput};
use wt_core::lifecycle::{DerivedPhase, Materialized, MaterializedKind, TreeState};
use wt_core::model::{RelPath, Target, TreeIdentity, TreeRec};
use wt_core::render::{Decision as RenderDecision, RenderRecord};
use wt_core::report::{Notice, NoticeLevel};
use wt_core::resource::ResourceState;
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, DoorToken, Mode, TreeToken};

use super::context::target_of;
use super::Context;

pub(crate) struct Door {
    pub target: Target,
    pub tree: TreeRec,
    pub identity: TreeIdentity,
    pub config: Config,
    pub env: EnvOutput,
    pub cwd: PathBuf,
    pub notices: Vec<Notice>,
    tree_token: Option<TreeToken>,
    door_token: Option<DoorToken>,
}

struct PrepareOptions {
    force_env: bool,
    check_all_tracked: bool,
    tree_token: Option<TreeToken>,
    door_token: Option<DoorToken>,
    state: Option<TreeState>,
}

impl Door {
    pub fn inherited_fds(&self) -> Vec<std::os::fd::RawFd> {
        [
            self.tree_token.as_ref().map(TreeToken::raw_fd),
            self.door_token.as_ref().map(DoorToken::raw_fd),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub fn release_gate(&mut self) {
        self.door_token.take();
        self.tree_token.take();
    }

    pub fn emit_notices(&self, context: &Context) {
        for notice in &self.notices {
            if notice.code == "BIN_DIR_MISSING"
                || (!context.quiet && (context.tty.stderr || context.verbose))
            {
                eprintln!("wt: {} — {}", notice.code, notice.message);
            }
        }
    }
}

pub(crate) fn enter(
    context: &mut Context,
    address: Option<&str>,
    verb: &str,
    force_env: bool,
) -> Result<Door, CoreError> {
    let target = context.resolve(address)?;
    let tree = context.tree(&target)?;
    // D0 must precede all mutation and process execution (SPEC §9.1).
    context.require_identity(&tree)?;
    let state = context.read_state(&target)?;
    let state_view = state.as_ref().map(|state| wt_core::lifecycle::StateView {
        phase: state.phase,
        op: state.op.as_ref().map(|operation| operation.verb),
        verify_pending: state.verify_pending,
        has_records: !state.resources.is_empty(),
    });
    let consult_lock = state_view.as_ref().is_some_and(|state| {
        !matches!(
            state.phase,
            wt_core::lifecycle::StatePhase::Ready | wt_core::lifecycle::StatePhase::Failed
        )
    });
    let phase = wt_core::derive_phase(&wt_core::lifecycle::TreeObs {
        entry: true,
        dir_exists: true,
        identity_ok: true,
        state: state_view,
        lock_held: consult_lock && lock::is_held(&context.tree_lock_path(&target))?,
        git_knows: true,
    })
    .phase;
    if !matches!(phase, DerivedPhase::Ready | DerivedPhase::Failed) {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "TREE_BUSY",
            format!("{target} is in phase {phase:?}"),
            "wait for the lifecycle operation or run its documented resume verb",
        ));
    }
    let holder = context.holder(target.to_string(), verb)?;
    let tree_token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Shared,
        &holder,
        Duration::ZERO,
    )?;
    let door_token = lock::door(&context.door_lock_path(&target), &holder)?;

    let mut door = prepare(
        context,
        target,
        tree,
        &holder,
        PrepareOptions {
            force_env,
            check_all_tracked: false,
            tree_token: Some(tree_token),
            door_token: Some(door_token),
            state,
        },
    )?;
    if phase == DerivedPhase::Failed {
        let notice = Notice {
            level: NoticeLevel::Warn,
            code: "TREE_NOT_READY".to_owned(),
            subject: Some(door.target.to_string()),
            message: "tree is usable but its last lifecycle operation failed".to_owned(),
        };
        context.pending_notices.push(notice.clone());
        door.notices.push(notice);
    }
    Ok(door)
}

/// Runs D3-D6 while the caller retains the lifecycle verb's exclusive tree lock.
pub(crate) fn enter_held(
    context: &mut Context,
    tree: TreeRec,
    verb: &str,
    force_env: bool,
    check_all_tracked: bool,
) -> Result<Door, CoreError> {
    let target = target_of(&tree);
    let holder = context.holder(target.to_string(), verb)?;
    let state = context.read_state(&target)?;
    prepare(
        context,
        target,
        tree,
        &holder,
        PrepareOptions {
            force_env,
            check_all_tracked,
            tree_token: None,
            door_token: None,
            state,
        },
    )
}

fn prepare(
    context: &mut Context,
    target: Target,
    mut tree: TreeRec,
    holder: &wt_sys::lock::Holder,
    options: PrepareOptions,
) -> Result<Door, CoreError> {
    let PrepareOptions {
        force_env,
        check_all_tracked,
        tree_token,
        door_token,
        state,
    } = options;
    let config = context.load_config(&tree)?;
    let appended = wt_core::ports::append(&tree.ports, &config.ports, tree.geometry.stride)?;
    if !appended.appended.is_empty() {
        let ports = appended.ports;
        context.mutate_registry(holder, |registry| {
            let record = registry
                .trees
                .iter_mut()
                .find(|record| record.tree_id == tree.tree_id)
                .ok_or_else(|| {
                    CoreError::new(
                        ExitClass::Conflict,
                        "TREE_REPLACED",
                        "tree incarnation changed while appending ports",
                        "retry the door against the current tree",
                    )
                })?;
            record.ports.clone_from(&ports);
            Ok(())
        })?;
        tree.ports = ports;
    }

    let identity = context.tree_identity(&tree)?;
    let effective = effective_for_cwd(context, &tree, &config)?;
    let existing_dirs = effective
        .bin
        .iter()
        .map(|path| Path::new(tree.path.as_str()).join(path.as_str()))
        .filter(|path| {
            matches!(
                wt_sys::fsx::path_kind(path),
                Ok(wt_sys::fsx::PathKind::Directory)
            )
        })
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    let file_sources = file_sources(context, &tree, &effective)?;
    let contributed = present_resource_env_from(context, &target, state.as_ref())?;
    let env = wt_core::assemble(EnvInputs {
        cfg: &effective,
        tree: &identity,
        home: &context.home.to_string_lossy(),
        contributed,
        task: None,
        parent: &context.parent_env,
        existing_dirs: &existing_dirs,
        file_sources: &file_sources,
        force_env,
    })?;
    render(
        context,
        &tree,
        &env,
        holder,
        state.as_ref(),
        check_all_tracked,
    )?;
    let notices = env_notices(&target, &env);
    context.pending_notices.extend(notices.clone());
    let cwd = door_cwd(context, &tree);
    Ok(Door {
        target,
        tree,
        identity,
        config,
        env,
        cwd,
        notices,
        tree_token,
        door_token,
    })
}

pub(crate) fn present_resource_env(
    context: &Context,
    target: &Target,
) -> Result<Vec<(wt_core::resource::ResourceKey, wt_core::model::EnvMap)>, CoreError> {
    let state = context.read_state(target)?;
    present_resource_env_from(context, target, state.as_ref())
}

fn present_resource_env_from(
    context: &Context,
    target: &Target,
    state: Option<&TreeState>,
) -> Result<Vec<(wt_core::resource::ResourceKey, wt_core::model::EnvMap)>, CoreError> {
    let mut records = state
        .into_iter()
        .flat_map(|state| state.resources.values().cloned())
        .collect::<Vec<_>>();
    let repo_path = context
        .home
        .join(wt_core::model::repo_state_path(&target.label));
    records.extend(
        wt_sys::fsx::read_json::<wt_core::lifecycle::RepoState>(&repo_path, "STATE_CORRUPT")?
            .into_iter()
            .flat_map(|state| state.resources.into_values()),
    );
    let mut contributed = records
        .into_iter()
        .filter(|record| record.state == ResourceState::Present)
        .map(|record| {
            let env = record
                .effective_snapshot()
                .env
                .iter()
                .filter(|(key, _)| !key.starts_with("WT_") && key.as_str() != "PATH")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            (record.key, env)
        })
        .collect::<Vec<_>>();
    contributed.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(contributed)
}

fn env_notices(target: &Target, output: &EnvOutput) -> Vec<Notice> {
    let subject = Some(target.to_string());
    let mut notices = output
        .report
        .missing_bins
        .iter()
        .map(|path| Notice {
            level: NoticeLevel::Warn,
            code: "BIN_DIR_MISSING".to_owned(),
            subject: subject.clone(),
            message: format!("declared bin directory {path} is missing"),
        })
        .collect::<Vec<_>>();
    if output.report.activation_ignored {
        notices.push(Notice {
            level: NoticeLevel::Warn,
            code: "ACTIVATION_IGNORED".to_owned(),
            subject: subject.clone(),
            message: "invalid WT_ACTIVATION metadata was ignored".to_owned(),
        });
    }
    notices.extend(output.report.kept.iter().map(|key| Notice {
        level: NoticeLevel::Info,
        code: "ENV_KEPT".to_owned(),
        subject: subject.clone(),
        message: format!("kept the caller's value for {key}"),
    }));
    notices.sort_by(|left, right| {
        (&left.level, &left.code, &left.subject, &left.message).cmp(&(
            &right.level,
            &right.code,
            &right.subject,
            &right.message,
        ))
    });
    notices
}

fn effective_for_cwd(
    context: &Context,
    tree: &TreeRec,
    config: &Config,
) -> Result<config::EffectiveScope, CoreError> {
    let root = Path::new(tree.path.as_str());
    let scope = context
        .cwd
        .strip_prefix(root)
        .ok()
        .and_then(|path| path.to_str())
        .filter(|path| !path.is_empty())
        .unwrap_or(".");
    config::effective_scope(config, scope)
}

fn file_sources(
    _context: &Context,
    tree: &TreeRec,
    effective: &config::EffectiveScope,
) -> Result<BTreeMap<String, String>, CoreError> {
    let canonical = Path::new(tree.path.as_str());
    effective
        .files
        .values()
        .filter_map(|file| file.source.as_ref())
        .map(|source| {
            let value =
                wt_sys::fsx::read_string(&canonical.join(source.as_str()))?.ok_or_else(|| {
                    CoreError::new(
                        ExitClass::State,
                        "FILE_SOURCE_MISSING",
                        format!("file source `{source}` is missing"),
                        "restore the source file or disable the rendered file",
                    )
                })?;
            Ok((source.as_str().to_owned(), value))
        })
        .collect()
}

fn render(
    context: &mut Context,
    tree: &TreeRec,
    output: &EnvOutput,
    holder: &wt_sys::lock::Holder,
    observed_state: Option<&TreeState>,
    check_all_tracked: bool,
) -> Result<(), CoreError> {
    if output.render.is_empty() {
        return Ok(());
    }
    let target = target_of(tree);
    let state = observed_state.cloned().ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "STATE_CORRUPT",
            "tree state is missing",
            "run `wt doctor` and repair the tree state",
        )
    })?;
    let paths_to_check = output
        .render
        .iter()
        .filter(|render| {
            check_all_tracked
                || !state
                    .materialized
                    .iter()
                    .any(|item| item.path == render.path)
        })
        .map(|render| PathBuf::from(&render.path))
        .collect::<Vec<_>>();
    let tracked = if paths_to_check.is_empty() {
        BTreeSet::new()
    } else {
        context
            .git(Path::new(tree.path.as_str()))?
            .tracked_paths(Path::new(tree.path.as_str()), &paths_to_check)?
    };
    let mut all_unchanged = true;
    for render in &output.render {
        let relative = RelPath::new(&render.path)?;
        let record = state
            .materialized
            .iter()
            .find(|item| item.path == render.path && item.kind == MaterializedKind::Rendered)
            .map(|item| RenderRecord {
                hash: item.hash.clone(),
            });
        let observed = wt_sys::fsx::observe_nofollow(
            Path::new(tree.path.as_str()),
            &relative,
            tracked.contains(&PathBuf::from(&render.path)),
        )?;
        if wt_core::render::decide(&observed, record.as_ref(), render.content.as_bytes())?
            != RenderDecision::Unchanged
        {
            all_unchanged = false;
            break;
        }
    }
    if all_unchanged {
        for render in &output.render {
            wt_sys::fsx::trace_budget(
                "render_hash_compare",
                Some(&Path::new(tree.path.as_str()).join(render.path.as_str())),
            )?;
        }
        return Ok(());
    }
    let _state_lock = lock::state_rmw(
        &context.state_lock_path(&target),
        holder,
        context.rmw_wait(),
    )?;
    let mut state =
        wt_sys::fsx::read_json::<TreeState>(&context.state_path(&target), "STATE_CORRUPT")?
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::State,
                    "STATE_CORRUPT",
                    "tree state is missing",
                    "run `wt doctor` and repair the tree state",
                )
            })?;
    let checked_at = wt_sys::fsx::timestamp()?;
    let mut changed_set = false;
    let mut changed_bytes = false;
    for render in &output.render {
        let relative = RelPath::new(&render.path)?;
        let record = state
            .materialized
            .iter()
            .find(|item| item.path == render.path && item.kind == MaterializedKind::Rendered)
            .map(|item| RenderRecord {
                hash: item.hash.clone(),
            });
        let observed = wt_sys::fsx::observe_nofollow(
            Path::new(tree.path.as_str()),
            &relative,
            tracked.contains(&PathBuf::from(&render.path)),
        )?;
        wt_sys::fsx::trace_budget(
            "render_hash_compare",
            Some(&Path::new(tree.path.as_str()).join(render.path.as_str())),
        )?;
        match wt_core::render::decide(&observed, record.as_ref(), render.content.as_bytes())? {
            RenderDecision::Unchanged => {}
            RenderDecision::Write => {
                let mode = u32::from_str_radix(&render.mode, 8).map_err(|_| {
                    CoreError::new(
                        ExitClass::State,
                        "CONFIG_INVALID",
                        "render mode is invalid",
                        "use a four-digit octal mode",
                    )
                })?;
                wt_sys::fsx::write_nofollow(
                    Path::new(tree.path.as_str()),
                    &relative,
                    render.content.as_bytes(),
                    mode,
                )?;
                wt_sys::failpoint::render_write()?;
                changed_bytes = true;
                let hash = wt_core::render::content_hash(render.content.as_bytes());
                if let Some(item) = state
                    .materialized
                    .iter_mut()
                    .find(|item| item.path == render.path)
                {
                    item.kind = MaterializedKind::Rendered;
                    item.hash = Some(hash);
                } else {
                    state.materialized.push(Materialized {
                        path: render.path.clone(),
                        kind: MaterializedKind::Rendered,
                        hash: Some(hash),
                        tracked_checked_at: checked_at.clone(),
                    });
                    changed_set = true;
                }
            }
        }
    }
    if changed_bytes {
        state
            .materialized
            .sort_by(|left, right| left.path.cmp(&right.path));
        wt_sys::fsx::write_json(&context.state_path(&target), &state)?;
    }
    drop(_state_lock);
    if changed_set {
        recompute_exclude(context, &tree.label)?;
    }
    Ok(())
}

pub(crate) fn recompute_exclude(
    context: &Context,
    label: &wt_core::model::Label,
) -> Result<(), CoreError> {
    let holder = context.holder(label.to_string(), "exclude")?;
    let _registry_lock = lock::registry_rmw(
        &context.home.join("registry.lock"),
        &holder,
        context.rmw_wait(),
    )?;
    let registry = wt_sys::fsx::read_json::<wt_core::model::Registry>(
        &context.home.join("registry.json"),
        "REGISTRY_CORRUPT",
    )?
    .unwrap_or_else(|| context.registry.clone());
    registry.validate()?;
    let mut paths = vec![".wt/".to_owned(), "**/.wt-tmp-*".to_owned()];
    for tree in registry.trees.iter().filter(|tree| &tree.label == label) {
        if let Some(state) = context.read_state(&target_of(tree))? {
            paths.extend(state.materialized.into_iter().map(|item| item.path));
        }
    }
    paths.extend(
        registry
            .tombstones
            .iter()
            .filter(|tombstone| &tombstone.label == label)
            .flat_map(|tombstone| tombstone.materialized.iter().map(ToString::to_string)),
    );
    let common = Path::new(registry.labels[label].common_gitdir.as_str());
    wt_sys::fsx::splice_exclude(
        &common.join("info/exclude"),
        &wt_core::exclude::block(paths),
    )?;
    Ok(())
}

fn door_cwd(context: &Context, tree: &TreeRec) -> PathBuf {
    let root = Path::new(tree.path.as_str());
    if context.cwd.starts_with(root) {
        context.cwd.clone()
    } else {
        root.to_path_buf()
    }
}
