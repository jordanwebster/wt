use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wt_core::config::{self, Layer, TiedTo};
use wt_core::lifecycle::{Operation, StatePhase, TreeState};
use wt_core::model::{
    gitdir_id, AbsPath, Label, LabelRec, SourceKind, Target, TreeRec, TreeSource,
};
use wt_core::report::{DeclaredReport, DeclaredResourceReport, RegisterData};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, Mode};

use crate::cli::Register;

use super::{door, executor, list, Context, Output};

pub(crate) fn run(context: &mut Context, args: Register) -> Result<Output, CoreError> {
    perform(
        context,
        args.path,
        args.label,
        args.move_to,
        args.repair,
        false,
    )
}

pub(crate) fn perform(
    context: &mut Context,
    path: PathBuf,
    label_arg: Option<String>,
    move_to: Option<PathBuf>,
    repair: bool,
    cloned: bool,
) -> Result<Output, CoreError> {
    if repair && move_to.is_some() {
        return Err(CoreError::new(
            ExitClass::Usage,
            "REPAIR_REFUSED",
            "--repair cannot be combined with --move-to",
            "use --repair at the registered path or --move-to after moving the checkout",
        ));
    }
    if let Some(destination) = move_to {
        return move_existing(context, path, label_arg, destination);
    }
    if matches!(
        wt_sys::fsx::path_kind(&path)?,
        wt_sys::fsx::PathKind::Symlink
    ) {
        return Err(CoreError::new(
            ExitClass::State,
            "ROOT_IS_SYMLINK",
            format!("{} is a symlink", path.display()),
            "register the real checkout path",
        ));
    }
    let path = wt_sys::fsx::canonicalize(&path)?;
    let git = context.git(&path)?;
    let top = wt_sys::fsx::canonicalize(&git.toplevel()?)?;
    if top != path {
        return Err(CoreError::new(
            ExitClass::State,
            "NOT_A_WORKTREE",
            "register path is not the repository root",
            "register the checkout root reported by git rev-parse --show-toplevel",
        ));
    }
    let common = wt_sys::fsx::canonicalize(&git.common_dir()?)?;
    let label = Label::new(label_arg.unwrap_or_else(|| default_label(&path)))?;
    let target = Target::canonical(label.clone());

    if repair {
        let existing = context
            .registry
            .trees
            .iter()
            .find(|tree| tree.canonical && tree.label == label)
            .cloned()
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::State,
                    "REPAIR_REFUSED",
                    format!("label {label} has no registered canonical checkout"),
                    "register the checkout without --repair",
                )
            })?;
        let registered = &context.registry.labels[&label];
        if existing.path.as_str() != path.to_string_lossy()
            || registered.path.as_str() != path.to_string_lossy()
            || registered.gitdir_id != gitdir_id(&common.to_string_lossy())
        {
            return Err(CoreError::new(
                ExitClass::State,
                "REPAIR_REFUSED",
                format!(
                    "{} is not {label}'s registered canonical checkout",
                    path.display()
                ),
                format!("run this command at {}", existing.path.as_str()),
            ));
        }
        let state = context.read_state(&target)?;
        if context.phase(&existing, state.as_ref())? != wt_core::lifecycle::DerivedPhase::Replaced {
            return Err(CoreError::new(
                ExitClass::State,
                "REPAIR_REFUSED",
                format!("{label}'s canonical checkout is not replaced"),
                "omit --repair for an ordinary idempotent registration",
            ));
        }
        return repair_canonical(context, existing);
    }

    if let Some(existing) = context
        .registry
        .trees
        .iter()
        .find(|tree| tree.canonical && tree.label == label)
        .cloned()
    {
        if existing.path.as_str() == path.to_string_lossy() && move_to.is_none() {
            if context.read_state(&target)?.is_some_and(|state| {
                state.phase == StatePhase::Initialising
                    && state
                        .op
                        .as_ref()
                        .is_some_and(|op| op.verb == wt_core::lifecycle::OpVerb::Register)
            }) {
                return resume_initialising(context, existing, cloned);
            }
            return finish(context, existing, false, false);
        }
        if let Some(destination) = move_to {
            let destination = wt_sys::fsx::canonicalize(&destination)?;
            if destination != path {
                return Err(CoreError::new(
                    ExitClass::State,
                    "PATH_REGISTERED",
                    "--move-to must name the checkout being registered",
                    "pass the checkout's new canonical path",
                ));
            }
        } else {
            return Err(CoreError::new(
                ExitClass::Conflict,
                "PATH_REGISTERED",
                format!(
                    "label {label} is already registered at {}",
                    existing.path.as_str()
                ),
                "use the existing label or choose another label",
            ));
        }
    }
    if let Some(other) = context
        .registry
        .trees
        .iter()
        .find(|tree| tree.path.as_str() == path.to_string_lossy())
    {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "PATH_REGISTERED",
            format!(
                "path is already registered as {}",
                super::context::target_of(other)
            ),
            "use the existing label",
        ));
    }
    let common_id = gitdir_id(&common.to_string_lossy());
    if let Some((other, _)) = context
        .registry
        .labels
        .iter()
        .find(|(_, record)| record.gitdir_id == common_id)
    {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "GITDIR_REGISTERED",
            format!("common gitdir is already registered as {other}"),
            format!("use `wt adopt {} --label {other}`", path.display()),
        ));
    }

    let holder = context.holder(target.to_string(), "register")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    let initial_config = initial_config(context, &path, &label)?;
    let coordinates = context.allocate(&target, &initial_config.ports)?;
    let now = wt_sys::fsx::timestamp()?;
    let tree_id = wt_sys::fsx::random_tree_id()?;
    let state = new_state(
        &target,
        &tree_id,
        StatePhase::Initialising,
        "register",
        &now,
    )?;
    context.write_state(&target, &state, &holder)?;
    let tree = TreeRec {
        tree_id: tree_id.clone(),
        label: label.clone(),
        name: "canonical".to_owned(),
        canonical: true,
        path: AbsPath::new(path.to_string_lossy().into_owned())?,
        slot: coordinates.slot,
        geometry: coordinates.geometry,
        ports: coordinates.ports,
        name_short: coordinates.name_short,
        session_name: coordinates.session_name,
        created_at: now.clone(),
        agent: None,
        source: TreeSource {
            kind: SourceKind::Canonical,
            branch: git.head_branch(&path)?,
            pr: None,
            start: None,
        },
    };
    context.mutate_registry(&holder, |registry| {
        registry.labels.insert(
            label.clone(),
            LabelRec {
                path: AbsPath::new(path.to_string_lossy().into_owned())?,
                gitdir_id: common_id.clone(),
                common_gitdir: AbsPath::new(common.to_string_lossy().into_owned())?,
                registered_at: now.clone(),
                trees_dir: None,
                default_branch: None,
            },
        );
        registry.trees.push(tree.clone());
        Ok(())
    })?;
    let identity = wt_core::model::RelPath::new(".wt/tree_id")?;
    wt_sys::fsx::write_nofollow(&path, &identity, format!("{tree_id}\n").as_bytes(), 0o600)?;
    door::recompute_exclude(context, &label)?;
    let prepared = door::enter_held(context, tree.clone(), "register", false, true)?;
    executor::refresh_all_declarations(context, &prepared)?;
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Ready;
        state.op = None;
        Ok(())
    })?;
    drop(token);
    finish_with_door(context, tree, true, cloned, Some(prepared), false)
}

fn repair_canonical(context: &mut Context, tree: TreeRec) -> Result<Output, CoreError> {
    let target = super::context::target_of(&tree);
    let holder = context.holder(target.to_string(), "register")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    wt_sys::fsx::write_nofollow(
        Path::new(tree.path.as_str()),
        &wt_core::model::RelPath::new(".wt/tree_id")?,
        format!("{}\n", tree.tree_id).as_bytes(),
        0o600,
    )?;
    door::recompute_exclude(context, &tree.label)?;
    let prepared = door::repair_held(context, tree.clone(), "register")?;
    drop(token);
    finish_with_door(context, tree, false, false, Some(prepared), false)
}

fn resume_initialising(
    context: &mut Context,
    tree: TreeRec,
    cloned: bool,
) -> Result<Output, CoreError> {
    let target = super::context::target_of(&tree);
    let holder = context.holder(target.to_string(), "register")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    wt_sys::fsx::write_nofollow(
        Path::new(tree.path.as_str()),
        &wt_core::model::RelPath::new(".wt/tree_id")?,
        format!("{}\n", tree.tree_id).as_bytes(),
        0o600,
    )?;
    door::recompute_exclude(context, &tree.label)?;
    let prepared = door::enter_held(context, tree.clone(), "register", false, true)?;
    executor::refresh_all_declarations(context, &prepared)?;
    context.mutate_state(&target, &holder, |state| {
        state.phase = StatePhase::Ready;
        state.op = None;
        Ok(())
    })?;
    drop(token);
    finish_with_door(context, tree, false, cloned, Some(prepared), false)
}

fn move_existing(
    context: &mut Context,
    old_path: PathBuf,
    label_arg: Option<String>,
    destination: PathBuf,
) -> Result<Output, CoreError> {
    if matches!(
        wt_sys::fsx::path_kind(&destination)?,
        wt_sys::fsx::PathKind::Symlink
    ) {
        return Err(CoreError::new(
            ExitClass::State,
            "ROOT_IS_SYMLINK",
            format!("{} is a symlink", destination.display()),
            "move the checkout to a real directory",
        ));
    }
    let destination = wt_sys::fsx::canonicalize(&destination)?;
    let label = if let Some(label) = label_arg {
        Label::new(label)?
    } else if let Ok(old_path) = wt_sys::fsx::canonicalize(&old_path) {
        context
            .registry
            .labels
            .iter()
            .find(|(_, record)| record.path.as_str() == old_path.to_string_lossy())
            .map(|(label, _)| label.clone())
            .unwrap_or(Label::new(default_label(&destination))?)
    } else {
        Label::new(default_label(&destination))?
    };
    let prior_label = context
        .registry
        .labels
        .get(&label)
        .cloned()
        .ok_or_else(|| {
            CoreError::new(
                ExitClass::NotFound,
                "NOT_FOUND",
                format!("label {label} is not registered"),
                "pass the registered label with --label",
            )
        })?;
    let mut tree = context
        .registry
        .trees
        .iter()
        .find(|tree| tree.canonical && tree.label == label)
        .cloned()
        .ok_or_else(|| super::context::not_found(&Target::canonical(label.clone())))?;
    let git = context.git(&destination)?;
    let target = Target::canonical(label.clone());
    let holder = context.holder(target.to_string(), "register")?;
    let _tree_lock = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    let _git_lock = lock::git(
        &context.git_lock_path(&prior_label.gitdir_id),
        &holder,
        super::context::duration(
            context.settings.locks.repo_git.as_deref(),
            std::time::Duration::from_secs(60),
        ),
    )?;
    git.worktree_repair(std::slice::from_ref(&destination))?;

    let top = wt_sys::fsx::canonicalize(&git.toplevel()?)?;
    let common = wt_sys::fsx::canonicalize(&git.common_dir()?)?;
    let listed = git.worktrees()?.into_iter().any(|worktree| {
        wt_sys::fsx::canonicalize(&worktree.path).is_ok_and(|path| path == destination)
    });
    if top != destination || !listed {
        return Err(CoreError::new(
            ExitClass::State,
            "NOT_A_WORKTREE",
            "git worktree repair did not restore the moved checkout",
            "inspect `git worktree list --porcelain` and repair the checkout",
        ));
    }
    let common_id = gitdir_id(&common.to_string_lossy());
    if context.registry.labels.iter().any(|(other, record)| {
        other != &label
            && (record.path.as_str() == destination.to_string_lossy()
                || record.gitdir_id == common_id)
    }) || context.registry.trees.iter().any(|record| {
        record.label != label && record.path.as_str() == destination.to_string_lossy()
    }) {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "PATH_REGISTERED",
            "the repaired path or gitdir belongs to another registration",
            "choose the existing label or move the checkout elsewhere",
        ));
    }
    tree.path = AbsPath::new(destination.to_string_lossy().into_owned())?;
    tree.source.branch = git.head_branch(&destination)?;
    context.mutate_registry(&holder, |registry| {
        let label_record = registry.labels.get_mut(&label).ok_or_else(|| {
            CoreError::new(
                ExitClass::NotFound,
                "NOT_FOUND",
                format!("label {label} disappeared during repair"),
                "retry the registration repair",
            )
        })?;
        label_record.path = tree.path.clone();
        label_record.common_gitdir = AbsPath::new(common.to_string_lossy().into_owned())?;
        label_record.gitdir_id.clone_from(&common_id);
        let canonical = registry
            .trees
            .iter_mut()
            .find(|record| record.tree_id == tree.tree_id)
            .ok_or_else(|| super::context::not_found(&target))?;
        canonical.path = tree.path.clone();
        canonical.source.branch.clone_from(&tree.source.branch);
        Ok(())
    })?;
    context.require_identity(&tree)?;
    drop(_git_lock);
    drop(_tree_lock);
    finish(context, tree, false, false)
}

pub(crate) fn finish(
    context: &mut Context,
    tree: TreeRec,
    registered: bool,
    _cloned: bool,
) -> Result<Output, CoreError> {
    finish_with_door(context, tree, registered, _cloned, None, true)
}

fn finish_with_door(
    context: &mut Context,
    tree: TreeRec,
    registered: bool,
    _cloned: bool,
    prepared: Option<door::Door>,
    refresh_declarations: bool,
) -> Result<Output, CoreError> {
    context.reload_registry()?;
    let had_prepared = prepared.is_some();
    let door = if let Some(door) = prepared {
        door
    } else {
        let door = match door::enter(
            context,
            Some(&super::context::target_of(&tree).to_string()),
            "register",
            false,
        ) {
            Ok(door) => door,
            Err(error) if error.code.0 == "CONFIG_INVALID" => {
                return invalid_config_report(context, tree, registered, &error.message)
            }
            Err(error) => return Err(error),
        };
        if refresh_declarations {
            executor::refresh_all_declarations(context, &door)?;
        }
        door
    };
    if had_prepared && refresh_declarations {
        executor::refresh_all_declarations(context, &door)?;
    }
    let config = door.config.clone();
    let catalog = context.task_catalog(&tree, &config)?;
    let mut resources = catalog
        .values()
        .filter(|node| node.destroy.is_some())
        .map(|node| DeclaredResourceReport {
            scope: node.scope.to_string(),
            task: node.id.clone(),
            tied_to: match node.tied_to.unwrap_or(TiedTo::Tree) {
                TiedTo::Tree => "tree",
                TiedTo::Repo => "repo",
            }
            .to_owned(),
            snapshot_keys: node.snapshot_env.clone(),
        })
        .collect::<Vec<_>>();
    resources.sort_by(|left, right| (&left.scope, &left.task).cmp(&(&right.scope, &right.task)));
    let mut tasks = catalog
        .keys()
        .filter(|id| !id.starts_with('@'))
        .cloned()
        .collect::<Vec<_>>();
    tasks.sort();
    let data = RegisterData {
        label: tree.label.to_string(),
        path: tree.path.as_str().to_owned(),
        gitdir_id: context.registry.labels[&tree.label].gitdir_id.clone(),
        registered,
        resumed: false,
        tree: list::tree_report(context, &tree, false, false, false)?,
        declared: DeclaredReport {
            tasks,
            resources,
            env: config.root.env.keys().cloned().collect(),
            files: config.root.files.keys().cloned().collect(),
            bin: config.root.bin.iter().map(ToString::to_string).collect(),
            ports: config.ports.iter().map(ToString::to_string).collect(),
            copy: config.root.copy.iter().map(ToString::to_string).collect(),
        },
        config_errors: Vec::new(),
    };
    Ok(Output::data(data)?.with_notices(door.notices))
}

fn invalid_config_report(
    context: &mut Context,
    tree: TreeRec,
    registered: bool,
    message: &str,
) -> Result<Output, CoreError> {
    let mut parts = message.splitn(4, ':');
    let error = wt_core::report::ConfigErrorReport {
        path: parts.next().unwrap_or("<config>").to_owned(),
        line: parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        col: parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        message: parts.next().unwrap_or(message).trim().to_owned(),
    };
    Output::data(RegisterData {
        label: tree.label.to_string(),
        path: tree.path.as_str().to_owned(),
        gitdir_id: context.registry.labels[&tree.label].gitdir_id.clone(),
        registered,
        resumed: false,
        tree: list::tree_report(context, &tree, false, false, false)?,
        declared: DeclaredReport {
            tasks: Vec::new(),
            resources: Vec::new(),
            env: Vec::new(),
            files: Vec::new(),
            bin: Vec::new(),
            ports: Vec::new(),
            copy: Vec::new(),
        },
        config_errors: vec![error],
    })
}

pub(crate) fn new_state(
    target: &Target,
    tree_id: &str,
    phase: StatePhase,
    verb: &str,
    now: &str,
) -> Result<TreeState, CoreError> {
    let op_verb = match verb {
        "register" => wt_core::lifecycle::OpVerb::Register,
        "adopt" => wt_core::lifecycle::OpVerb::Adopt,
        "new" => wt_core::lifecycle::OpVerb::New,
        "sync" => wt_core::lifecycle::OpVerb::Sync,
        "remove" => wt_core::lifecycle::OpVerb::Remove,
        _ => {
            return Err(CoreError::new(
                ExitClass::Internal,
                "OP_INVALID",
                "invalid lifecycle operation",
                "report this wt bug",
            ))
        }
    };
    Ok(TreeState {
        schema: 1,
        tree_id: tree_id.to_owned(),
        label: target.label.clone(),
        name: target.name.clone(),
        phase,
        op: Some(Operation {
            verb: op_verb,
            pid: std::process::id(),
            started: now.to_owned(),
        }),
        verify_pending: false,
        sync: None,
        verify: None,
        resources: BTreeMap::new(),
        materialized: Vec::new(),
        last_error: None,
    })
}

pub(crate) fn initial_config(
    context: &Context,
    path: &Path,
    label: &Label,
) -> Result<config::Config, CoreError> {
    let repo_path = path.join(".wt.toml");
    let repo = wt_sys::fsx::read_string(&repo_path)?
        .map(|source| config::parse(&source, &repo_path.to_string_lossy()))
        .transpose()?
        .unwrap_or_default();
    let user = context
        .settings
        .repos
        .get(label)
        .cloned()
        .unwrap_or_default();
    let preliminary = config::merge(&[(Layer::Repo, repo.clone()), (Layer::User, user.clone())]);
    let mut adapter = config::Config::default();
    let mut hits = Vec::new();
    for scope in std::iter::once(".").chain(preliminary.dirs.keys().map(String::as_str)) {
        let relative = wt_core::model::RelPath::new(scope)?;
        let snapshot = wt_sys::fsx::capture_dir_snapshot(
            path,
            &relative,
            &[
                "package.json".to_owned(),
                "rustfmt.toml".to_owned(),
                ".rustfmt.toml".to_owned(),
            ],
        )?;
        let effective = config::effective_scope(&preliminary, scope)?;
        hits.extend(wt_core::adapters::detect(&snapshot, &effective.adapters)?);
    }
    wt_core::adapters::apply_contribution(&mut adapter, &wt_core::adapters::contribution(&hits)?)?;
    Ok(config::merge(&[
        (Layer::Adapter, adapter),
        (Layer::Repo, repo),
        (Layer::User, user),
    ]))
}

fn default_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo")
        .trim_end_matches(".git")
        .to_owned()
}
