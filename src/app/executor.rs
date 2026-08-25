use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wt_core::config::{self, Command, Config, EffectiveScope, TiedTo};
use wt_core::env::{EnvInputs, EnvOutput, TaskContext};
use wt_core::lifecycle::RepoState;
use wt_core::model::{duration_millis, scope_enc};
use wt_core::report::{ChildReport, DryRunData, DryRunStepReport, RunData, StepReport};
use wt_core::report::{Notice, NoticeLevel};
use wt_core::resource::{
    Action as ResourceDecision, EffectResult, Event, ExpandedCommand, Probe, ProbeResult,
    ResourceKey, ResourceRecord, ResourceSnapshot, SnapshotRoots,
};
use wt_core::task::{HeldLocks, Node, PlannedLock, TaskPlan};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, GitToken, NamedToken, ResourceToken};
use wt_sys::proc::{self, CommandRequest, ProcessOutput, Tee};
use wt_sys::snapshot::{Action as SnapshotAction, ExecuteResult, ExecutionOptions};

use super::context::duration;
use super::door::Door;
use super::Context;

enum Guard {
    Git { _token: GitToken },
    Resource { _token: ResourceToken },
    Named { _token: NamedToken },
}

pub(crate) struct PlanExecution {
    pub data: RunData,
    pub notices: Vec<Notice>,
}

struct NodeExecution {
    status: String,
    child: Option<ChildReport>,
    log: Option<String>,
    notices: Vec<Notice>,
}

pub(crate) fn plan(context: &Context, door: &Door, root: &str) -> Result<TaskPlan, CoreError> {
    let mut catalog = context.task_catalog(&door.tree, &door.config)?;
    for node in catalog.values_mut().filter(|node| node.destroy.is_some()) {
        let tied_to = node.tied_to.ok_or_else(|| {
            CoreError::new(
                ExitClass::State,
                "CONFIG_INVALID",
                format!("resource {} has no tied_to", node.id),
                "set tied_to to tree or repo",
            )
        })?;
        node.resource = Some(ResourceKey {
            label: door.tree.label.clone(),
            tied_to,
            name: (tied_to == TiedTo::Tree).then(|| door.tree.name.clone()),
            scope: node.scope.clone(),
            task: node.id.clone(),
        });
    }
    wt_core::task::plan(root, &catalog)
}

pub(crate) fn dry_run(plan: &TaskPlan) -> DryRunData {
    DryRunData {
        task: plan.root.clone(),
        steps: plan
            .nodes
            .iter()
            .map(|node| DryRunStepReport {
                id: node.id.clone(),
                scope: node.scope.to_string(),
                origin: format!("{:?}", node.origin).to_ascii_lowercase(),
                cwd: node.cwd.to_string(),
                run: node.run.clone(),
                exists: node.exists.clone(),
                lock: node.lock.clone(),
                sys_locks: node.sys_locks.clone(),
                resource: node.resource.is_some(),
                tied_to: node.tied_to.map(|tied| match tied {
                    TiedTo::Tree => "tree".to_owned(),
                    TiedTo::Repo => "repo".to_owned(),
                }),
            })
            .collect(),
    }
}

pub(crate) fn execute_plan(
    context: &mut Context,
    door: &Door,
    plan: &TaskPlan,
    timeout: Option<&str>,
    wait: Option<&str>,
    no_log: bool,
) -> Result<PlanExecution, CoreError> {
    let mut steps = Vec::new();
    let mut notices = Vec::new();
    let mut last_child = None;
    let mut last_log = None;
    for (index, node) in plan.nodes.iter().enumerate() {
        let started = Instant::now();
        let _guards = acquire_node_locks(context, door, node, wait)?;
        let (status, child, log) = if node.resource.is_some() {
            refresh_node_declaration(context, door, node)?;
            let result = run_resource(context, door, node, timeout, no_log)?;
            notices.extend(result.notices);
            (result.status, result.child, result.log)
        } else {
            run_task(context, door, node, timeout, no_log)?
        };
        if child.is_some() {
            last_child.clone_from(&child);
        }
        if log.is_some() {
            last_log.clone_from(&log);
        }
        steps.push(StepReport {
            id: node.id.clone(),
            scope: node.scope.to_string(),
            status,
            child,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        if plan.root == "sync" && index + 1 < plan.nodes.len() {
            wt_sys::failpoint::sync_mid()?;
        }
    }
    Ok(PlanExecution {
        data: RunData {
            target: door.target.to_string(),
            task: plan.root.clone(),
            child: last_child,
            log: last_log,
            steps,
        },
        notices,
    })
}

pub(crate) fn refresh_all_declarations(
    context: &mut Context,
    door: &Door,
) -> Result<(), CoreError> {
    let mut catalog = context.task_catalog(&door.tree, &door.config)?;
    for node in catalog.values_mut().filter(|node| node.destroy.is_some()) {
        let tied_to = node.tied_to.unwrap_or(TiedTo::Tree);
        node.resource = Some(ResourceKey {
            label: door.tree.label.clone(),
            tied_to,
            name: (tied_to == TiedTo::Tree).then(|| door.tree.name.clone()),
            scope: node.scope.clone(),
            task: node.id.clone(),
        });
        refresh_node_declaration(context, door, node)?;
    }
    Ok(())
}

pub(crate) fn probe_all_resources(context: &Context, door: &Door) -> Result<(), CoreError> {
    let records = context
        .read_state(&door.target)?
        .map(|state| state.resources.into_values().collect::<Vec<_>>())
        .unwrap_or_default();
    for record in records {
        let holder = context.holder(door.target.to_string(), "probe")?;
        let _resource = lock::resource(
            &resource_lock_path(context, &record.key),
            &holder,
            duration(
                context.settings.locks.resource.as_deref(),
                Duration::from_secs(120),
            ),
        )?;
        let probe = execute_probe(context, door, record.effective_snapshot())?;
        let step = wt_core::resource::step(Some(record), Event::Probe(probe));
        persist_step(
            context,
            door,
            &step.record.as_ref().expect("probe keeps record").key,
            &step.record,
        )?;
    }
    Ok(())
}

fn acquire_node_locks(
    context: &Context,
    door: &Door,
    node: &Node,
    wait: Option<&str>,
) -> Result<Vec<Guard>, CoreError> {
    let holder = context.holder(door.target.to_string(), "run")?;
    let mut guards = Vec::new();
    let plan = wt_core::task::lock_plan(
        node,
        HeldLocks {
            tree: true,
            repo_git: false,
        },
    );
    for lock_plan in plan {
        match lock_plan {
            PlannedLock::TreeShared => unreachable!("the door already owns the tree lock"),
            PlannedLock::RepoGit => guards.push(Guard::Git {
                _token: lock::git(
                    &context.git_lock_path(&context.registry.labels[&door.tree.label].gitdir_id),
                    &holder,
                    duration(
                        wait.or(context.settings.locks.repo_git.as_deref()),
                        Duration::from_secs(60),
                    ),
                )?,
            }),
            PlannedLock::Resource { key } => guards.push(Guard::Resource {
                _token: lock::resource(
                    &resource_lock_path(context, &key),
                    &holder,
                    duration(
                        wait.or(context.settings.locks.resource.as_deref()),
                        Duration::from_secs(120),
                    ),
                )?,
            }),
            PlannedLock::Named { name } => guards.push(Guard::Named {
                _token: lock::named(
                    &context
                        .home
                        .join(format!("locks/{}/named/{name}.lock", door.tree.label)),
                    &holder,
                    wait.and_then(duration_millis).map(Duration::from_millis),
                )?,
            }),
        }
    }
    Ok(guards)
}

fn resource_lock_path(context: &Context, key: &ResourceKey) -> PathBuf {
    let tied = match key.tied_to {
        TiedTo::Tree => format!("tree/{}/", key.name.as_deref().unwrap_or("canonical")),
        TiedTo::Repo => "repo/".to_owned(),
    };
    context.home.join(format!(
        "locks/{}/res/{tied}{}/{}.lock",
        key.label,
        scope_enc(&key.scope),
        key.task
    ))
}

fn run_task(
    context: &Context,
    door: &Door,
    node: &Node,
    timeout: Option<&str>,
    no_log: bool,
) -> Result<(String, Option<ChildReport>, Option<String>), CoreError> {
    let assembled = node_environment(context, door, node)?;
    let cwd = Path::new(door.tree.path.as_str()).join(node.cwd.as_str());
    if !matches!(
        wt_sys::fsx::path_kind(&cwd)?,
        wt_sys::fsx::PathKind::Directory
    ) {
        return Err(CoreError::new(
            ExitClass::State,
            "CWD_MISSING",
            format!("task cwd {} is missing", cwd.display()),
            "create the directory or correct task.cwd",
        ));
    }
    if let Some(exists) = &node.exists {
        let request = request(exists, &assembled, &cwd)?;
        let probe = proc::probe(
            &request,
            duration(
                node.timeout
                    .as_deref()
                    .or(context.settings.task.probe_timeout.as_deref()),
                Duration::from_secs(10),
            ),
            wt_sys::fsx::timestamp()?,
        );
        match probe.result {
            ProbeResult::Present => return Ok(("present".to_owned(), None, None)),
            ProbeResult::Absent => {}
            ProbeResult::Failed { .. } => {
                return Err(CoreError::new(
                    ExitClass::ChildFailed,
                    "TASK_PROBE_FAILED",
                    format!("probe for {} failed", node.id),
                    "fix the probe and retry",
                ))
            }
        }
    }
    let Some(run) = &node.run else {
        return Ok(("skipped".to_owned(), None, None));
    };
    let log = log_path(context, door, node, no_log)?;
    let output = proc::run(
        &request(run, &assembled, &cwd)?,
        log.as_deref(),
        timeout
            .or(node.timeout.as_deref())
            .or(context.settings.task.timeout.as_deref())
            .and_then(duration_millis)
            .map(Duration::from_millis),
        if context.json {
            Tee::Stderr
        } else {
            Tee::Inherit
        },
    )?;
    let child = child_report(&output);
    if !output.success() {
        return Err(child_failed("TASK_FAILED", &node.id, &output));
    }
    Ok((
        "ok".to_owned(),
        Some(child),
        log.map(|path| path.to_string_lossy().into_owned()),
    ))
}

fn run_resource(
    context: &Context,
    door: &Door,
    node: &Node,
    _timeout: Option<&str>,
    no_log: bool,
) -> Result<NodeExecution, CoreError> {
    let key = node.resource.as_ref().expect("resource node has a key");
    let mut record = load_record(context, door, key)?;
    let snapshot = record.effective_snapshot().clone();
    let probe = execute_probe(context, door, &snapshot)?;
    let first = wt_core::resource::step(
        Some(record),
        Event::Run {
            probe: probe.clone(),
            run: None,
            confirm: None,
        },
    );
    persist_step(context, door, key, &first.record)?;
    if let Some(error) = first.error {
        return Err(error);
    }
    if first.action != ResourceDecision::Run {
        return Ok(NodeExecution {
            status: resource_status(&first),
            child: None,
            log: None,
            notices: step_notices(&first, node),
        });
    }
    record = first.record.expect("run decision retains record");
    let log = log_path(context, door, node, no_log)?;
    let effect = execute_effect(
        context,
        door,
        record.effective_snapshot(),
        SnapshotAction::Run,
        log.as_deref(),
    )?;
    let child = effect_child(&effect);
    let confirm = execute_probe(context, door, record.effective_snapshot())?;
    let final_step = wt_core::resource::step(
        Some(record),
        Event::Run {
            probe,
            run: Some(effect),
            confirm: Some(confirm),
        },
    );
    persist_step(context, door, key, &final_step.record)?;
    if let Some(error) = final_step.error {
        return Err(error);
    }
    let notices = step_notices(&final_step, node);
    Ok(NodeExecution {
        status: resource_status(&final_step),
        child,
        log: log.map(|path| path.to_string_lossy().into_owned()),
        notices,
    })
}

fn step_notices(step: &wt_core::resource::StepResult, node: &Node) -> Vec<Notice> {
    step.notices
        .iter()
        .map(|code| Notice {
            level: NoticeLevel::Info,
            code: code.clone(),
            subject: Some(node.id.clone()),
            message: match code.as_str() {
                "RESOURCE_DECLARED_EXTERNAL" => "declared; created by the application".to_owned(),
                _ => format!("resource transition reported {code}"),
            },
            guidance: None,
        })
        .collect()
}

pub(crate) fn destroy_resource(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
    teardown: bool,
) -> Result<(String, Option<ChildReport>), CoreError> {
    let mut record = load_record(context, door, key)?;
    let probe = execute_probe(context, door, record.effective_snapshot())?;
    let first = wt_core::resource::step(
        Some(record),
        Event::Destroy {
            teardown,
            probe: probe.clone(),
            destroy: None,
            confirm: None,
        },
    );
    persist_step(context, door, key, &first.record)?;
    if first.action != ResourceDecision::Destroy {
        if let Some(error) = first.error {
            return Err(error);
        }
        return Ok((resource_status(&first), None));
    }
    record = first.record.expect("destroy decision retains record");
    let effect = execute_effect(
        context,
        door,
        record.effective_snapshot(),
        SnapshotAction::Destroy,
        None,
    )?;
    let child = effect_child(&effect);
    let confirm = execute_probe(context, door, record.effective_snapshot())?;
    let final_step = wt_core::resource::step(
        Some(record),
        Event::Destroy {
            teardown,
            probe,
            destroy: Some(effect),
            confirm: Some(confirm),
        },
    );
    persist_step(context, door, key, &final_step.record)?;
    if let Some(error) = final_step.error {
        return Err(error);
    }
    Ok((resource_status(&final_step), child))
}

pub(crate) fn destroy_stored_resource(
    context: &Context,
    target: &wt_core::model::Target,
    record: ResourceRecord,
    tree_replaced: bool,
) -> Result<(String, Option<ChildReport>), CoreError> {
    let key = record.key.clone();
    let probe = if matches!(
        record.state,
        wt_core::resource::ResourceState::Present | wt_core::resource::ResourceState::Orphaned
    ) {
        Probe::present(wt_sys::fsx::timestamp()?)
    } else {
        execute_stored_probe(context, &record, tree_replaced)?
    };
    let first = wt_core::resource::step(
        Some(record),
        Event::Destroy {
            teardown: true,
            probe: probe.clone(),
            destroy: None,
            confirm: None,
        },
    );
    persist_tree_record(context, target, &key, &first.record)?;
    if first.action != ResourceDecision::Destroy {
        if let Some(error) = first.error {
            return Err(error);
        }
        return Ok((resource_status(&first), None));
    }
    let record = first.record.expect("destroy decision retains record");
    let effect = execute_stored_effect(context, &record, tree_replaced)?;
    let child = effect_child(&effect);
    let confirm = execute_stored_probe(context, &record, tree_replaced)?;
    let final_step = wt_core::resource::step(
        Some(record),
        Event::Destroy {
            teardown: true,
            probe,
            destroy: Some(effect),
            confirm: Some(confirm),
        },
    );
    persist_tree_record(context, target, &key, &final_step.record)?;
    if let Some(error) = final_step.error {
        return Err(error);
    }
    Ok((resource_status(&final_step), child))
}

pub(crate) fn newest_resources_first(mut records: Vec<ResourceRecord>) -> Vec<ResourceRecord> {
    records.sort_by(|left, right| {
        right
            .effective_snapshot()
            .recorded_at
            .cmp(&left.effective_snapshot().recorded_at)
            .then_with(|| scoped_id(&right.key).cmp(&scoped_id(&left.key)))
    });
    records
}

fn execute_stored_probe(
    context: &Context,
    record: &ResourceRecord,
    tree_replaced: bool,
) -> Result<Probe, CoreError> {
    match wt_sys::snapshot::execute_observed(
        record.effective_snapshot(),
        SnapshotAction::Exists,
        &context.parent_env,
        Path::new(context.registry.labels[&record.key.label].path.as_str()),
        ExecutionOptions {
            tree_replaced,
            deadlines: wt_sys::snapshot::Deadlines::from_settings(&context.settings.task)?,
            log: None,
            tee: Tee::Quiet,
        },
    )? {
        ExecuteResult::Probe(probe) => Ok(probe),
        ExecuteResult::Orphaned { reason, .. } => {
            Ok(Probe::failed_spawn(wt_sys::fsx::timestamp()?, reason))
        }
        ExecuteResult::Child(_) => unreachable!("exists execution returns a probe"),
    }
}

fn execute_stored_effect(
    context: &Context,
    record: &ResourceRecord,
    tree_replaced: bool,
) -> Result<EffectResult, CoreError> {
    let at = wt_sys::fsx::timestamp()?;
    match wt_sys::snapshot::execute_observed(
        record.effective_snapshot(),
        SnapshotAction::Destroy,
        &context.parent_env,
        Path::new(context.registry.labels[&record.key.label].path.as_str()),
        ExecutionOptions {
            tree_replaced,
            deadlines: wt_sys::snapshot::Deadlines::from_settings(&context.settings.task)?,
            log: None,
            tee: if context.json {
                Tee::Stderr
            } else {
                Tee::Inherit
            },
        },
    )? {
        ExecuteResult::Child(output) if output.success() => Ok(EffectResult::Ok {
            at,
            child: Some(output.child),
        }),
        ExecuteResult::Child(output) => Ok(EffectResult::Failed {
            at,
            message: format!("child exited {}", output.mapped_exit()),
            reason: if output.timed_out {
                "timeout"
            } else {
                "child_failed"
            }
            .to_owned(),
            child: Some(output.child),
        }),
        ExecuteResult::Orphaned { reason, remedy, .. } => Ok(EffectResult::Failed {
            at,
            message: remedy,
            reason,
            child: None,
        }),
        ExecuteResult::Probe(_) => unreachable!("destroy execution does not return a probe"),
    }
}

fn persist_tree_record(
    context: &Context,
    target: &wt_core::model::Target,
    key: &ResourceKey,
    record: &Option<ResourceRecord>,
) -> Result<(), CoreError> {
    let holder = context.holder(target.to_string(), "resource")?;
    let id = scoped_id(key);
    context.mutate_state(target, &holder, |state| {
        if let Some(record) = record {
            state.resources.insert(id, record.clone());
        } else {
            state.resources.remove(&id);
        }
        Ok(())
    })
}

fn execute_probe(
    context: &Context,
    door: &Door,
    snapshot: &ResourceSnapshot,
) -> Result<Probe, CoreError> {
    match wt_sys::snapshot::execute_observed(
        snapshot,
        SnapshotAction::Exists,
        &context.parent_env,
        Path::new(context.registry.labels[&door.tree.label].path.as_str()),
        ExecutionOptions {
            tree_replaced: false,
            deadlines: wt_sys::snapshot::Deadlines::from_settings(&context.settings.task)?,
            log: None,
            tee: Tee::Quiet,
        },
    )? {
        ExecuteResult::Probe(probe) => Ok(probe),
        ExecuteResult::Orphaned { reason, .. } => {
            Ok(Probe::failed_spawn(wt_sys::fsx::timestamp()?, reason))
        }
        ExecuteResult::Child(_) => unreachable!("exists execution returns a probe"),
    }
}

fn execute_effect(
    context: &Context,
    door: &Door,
    snapshot: &ResourceSnapshot,
    action: SnapshotAction,
    log: Option<&Path>,
) -> Result<EffectResult, CoreError> {
    let at = wt_sys::fsx::timestamp()?;
    match wt_sys::snapshot::execute_observed(
        snapshot,
        action,
        &context.parent_env,
        Path::new(context.registry.labels[&door.tree.label].path.as_str()),
        ExecutionOptions {
            tree_replaced: false,
            deadlines: wt_sys::snapshot::Deadlines::from_settings(&context.settings.task)?,
            log,
            tee: if context.json {
                Tee::Stderr
            } else {
                Tee::Inherit
            },
        },
    )? {
        ExecuteResult::Child(output) if output.success() => Ok(EffectResult::Ok {
            at,
            child: Some(output.child),
        }),
        ExecuteResult::Child(output) => Ok(EffectResult::Failed {
            at,
            message: format!("child exited {}", output.mapped_exit()),
            reason: if output.timed_out {
                "timeout"
            } else {
                "child_failed"
            }
            .to_owned(),
            child: Some(output.child),
        }),
        ExecuteResult::Orphaned { reason, remedy, .. } => Ok(EffectResult::Failed {
            at,
            message: remedy,
            reason,
            child: None,
        }),
        ExecuteResult::Probe(_) => unreachable!("effect execution does not return a probe"),
    }
}

fn refresh_node_declaration(context: &Context, door: &Door, node: &Node) -> Result<(), CoreError> {
    let key = node.resource.as_ref().expect("resource node has a key");
    let assembled = match node_environment(context, door, node) {
        Ok(assembled) => assembled,
        Err(error) if error.code.0 == "ENV_UNDEFINED" => {
            let holder = context.holder(door.target.to_string(), "refresh")?;
            context.mutate_state(&door.target, &holder, |state| {
                state.last_error = Some(format!("REFRESH_SKIPPED:{}:{}", key.scope, key.task));
                Ok(())
            })?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let effective = config_for_node(&door.config, node)?;
    let bin_dirs = effective
        .bin
        .iter()
        .map(|path| Path::new(door.tree.path.as_str()).join(path.as_str()))
        .collect::<Vec<_>>();
    let bin_exes = wt_sys::snapshot::capture_bin_exes(&bin_dirs)?;
    let snapshot = ResourceSnapshot {
        schema: 1,
        key: key.clone(),
        name: assembled
            .env
            .get("WT_SELF")
            .cloned()
            .unwrap_or_else(|| wt_core::resource::default_name(key, &door.tree.name_short)),
        cwd_rel: node.cwd.clone(),
        exists: node
            .exists
            .as_ref()
            .map(|command| expanded(command, &assembled))
            .transpose()?,
        destroy: expanded(
            node.destroy.as_ref().expect("resource has destroy"),
            &assembled,
        )?,
        run: node
            .run
            .as_ref()
            .map(|command| expanded(command, &assembled))
            .transpose()?,
        env: wt_core::snapshot::minimise_env(
            &assembled.env,
            &effective.env.keys().cloned().collect::<Vec<_>>(),
            &node.env.keys().cloned().collect::<Vec<_>>(),
            &node.snapshot_env,
            key.tied_to,
        ),
        bin_dirs: bin_dirs
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        bin_exes,
        roots: SnapshotRoots {
            tree: door.tree.path.as_str().to_owned(),
            home: context.home.to_string_lossy().into_owned(),
        },
        recorded_at: wt_sys::fsx::timestamp()?,
    };
    store_declaration(context, door, key, snapshot)
}

fn store_declaration(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
    mut snapshot: ResourceSnapshot,
) -> Result<(), CoreError> {
    let holder = context.holder(door.target.to_string(), "refresh")?;
    match key.tied_to {
        TiedTo::Tree => context.mutate_state(&door.target, &holder, |state| {
            let id = scoped_id(key);
            let step = wt_core::resource::step(
                state.resources.remove(&id),
                Event::Declare(Box::new(snapshot)),
            );
            if let Some(record) = step.record {
                state.resources.insert(id, record);
            }
            Ok(())
        }),
        TiedTo::Repo => {
            wt_core::declarations::strip_tree_specific_env(&mut snapshot);
            let path = context
                .home
                .join(wt_core::model::repo_state_path(&key.label));
            let lock_path = context
                .home
                .join(format!("locks/{}/_repo.rmw.lock", key.label));
            let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
            let mut state =
                wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?.unwrap_or(RepoState {
                    schema: 1,
                    label: key.label.clone(),
                    resources: BTreeMap::new(),
                });
            let id = scoped_id(key);
            let step = wt_core::resource::step(
                state.resources.remove(&id),
                Event::Declare(Box::new(snapshot)),
            );
            if let Some(record) = step.record {
                state.resources.insert(id, record);
            }
            wt_sys::fsx::write_json(&path, &state)
        }
    }
}

fn load_record(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
) -> Result<ResourceRecord, CoreError> {
    let record = match key.tied_to {
        TiedTo::Tree => context
            .read_state(&door.target)?
            .and_then(|state| state.resources.get(&scoped_id(key)).cloned()),
        TiedTo::Repo => wt_sys::fsx::read_json::<RepoState>(
            &context
                .home
                .join(wt_core::model::repo_state_path(&key.label)),
            "STATE_CORRUPT",
        )?
        .and_then(|state| state.resources.get(&scoped_id(key)).cloned()),
    };
    record.ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "RESOURCE_RECORD_MISSING",
            format!("resource {} has no declaration record", key.task),
            "run the task again to refresh its declaration",
        )
    })
}

fn persist_step(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
    record: &Option<ResourceRecord>,
) -> Result<(), CoreError> {
    let holder = context.holder(door.target.to_string(), "resource")?;
    let id = scoped_id(key);
    match key.tied_to {
        TiedTo::Tree => context.mutate_state(&door.target, &holder, |state| {
            match record {
                Some(record) => {
                    state.resources.insert(id, record.clone());
                }
                None => {
                    state.resources.remove(&id);
                }
            }
            Ok(())
        }),
        TiedTo::Repo => {
            let path = context
                .home
                .join(wt_core::model::repo_state_path(&key.label));
            let lock_path = context
                .home
                .join(format!("locks/{}/_repo.rmw.lock", key.label));
            let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
            let mut state =
                wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?.unwrap_or(RepoState {
                    schema: 1,
                    label: key.label.clone(),
                    resources: BTreeMap::new(),
                });
            match record {
                Some(record) => {
                    state.resources.insert(id, record.clone());
                }
                None => {
                    state.resources.remove(&id);
                }
            }
            wt_sys::fsx::write_json(&path, &state)
        }
    }
}

fn node_environment(context: &Context, door: &Door, node: &Node) -> Result<EnvOutput, CoreError> {
    let effective = config_for_node(&door.config, node)?;
    let existing_dirs = effective
        .bin
        .iter()
        .map(|path| Path::new(door.tree.path.as_str()).join(path.as_str()))
        .filter(|path| {
            matches!(
                wt_sys::fsx::path_kind(path),
                Ok(wt_sys::fsx::PathKind::Directory)
            )
        })
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    let mut sources = BTreeMap::new();
    for source in effective
        .files
        .values()
        .filter_map(|file| file.source.as_ref())
    {
        if let Some(value) =
            wt_sys::fsx::read_string(&Path::new(door.tree.path.as_str()).join(source.as_str()))?
        {
            sources.insert(source.as_str().to_owned(), value);
        }
    }
    let resource_name = node.resource.as_ref().map(|key| {
        node.name
            .clone()
            .unwrap_or_else(|| wt_core::resource::default_name(key, &door.tree.name_short))
    });
    wt_core::assemble(EnvInputs {
        cfg: &effective,
        tree: &door.identity,
        home: &context.home.to_string_lossy(),
        contributed: super::door::present_resource_env(context, &door.target)?,
        task: Some(TaskContext {
            id: node.id.clone(),
            env: node.env.clone(),
            resource_name,
        }),
        parent: &context.parent_env,
        existing_dirs: &existing_dirs,
        file_sources: &sources,
    })
}

fn config_for_node(config: &Config, node: &Node) -> Result<EffectiveScope, CoreError> {
    config::effective_scope(config, node.scope.as_str())
}

fn expanded(command: &Command, assembled: &EnvOutput) -> Result<ExpandedCommand, CoreError> {
    let context = wt_core::template::Context {
        vars: &assembled.vars,
        functions: &assembled.functions,
    };
    match command {
        Command::Shell(shell) => Ok(ExpandedCommand::Shell {
            shell: shell.clone(),
        }),
        Command::Argv(argv) => argv
            .iter()
            .map(|value| wt_core::template::expand(value, &context))
            .collect::<Result<Vec<_>, _>>()
            .map(|argv| ExpandedCommand::Argv { argv }),
    }
}

fn request(
    command: &Command,
    assembled: &EnvOutput,
    cwd: &Path,
) -> Result<CommandRequest, CoreError> {
    CommandRequest::expanded(&expanded(command, assembled)?, cwd, assembled.env.clone())
}

fn log_path(
    context: &Context,
    door: &Door,
    node: &Node,
    no_log: bool,
) -> Result<Option<PathBuf>, CoreError> {
    if no_log {
        return Ok(None);
    }
    let dir = Path::new(door.tree.path.as_str()).join(".wt/logs");
    wt_sys::fsx::create_private_dir(&dir)?;
    wt_sys::fsx::retain_logs(
        &dir,
        node.scope.as_str(),
        &node.id,
        context.settings.logs.keep.saturating_sub(1),
    )?;
    let stamp = wt_sys::fsx::timestamp()?.replace(['.', ':'], "-");
    Ok(Some(dir.join(format!(
        "{}-{}-{stamp}.log",
        scope_enc(&node.scope),
        node.id.replace('/', "_")
    ))))
}

fn child_report(output: &ProcessOutput) -> ChildReport {
    ChildReport {
        code: output.child.code,
        signal: output.child.signal,
    }
}

fn effect_child(effect: &EffectResult) -> Option<ChildReport> {
    let child = match effect {
        EffectResult::Ok { child, .. } | EffectResult::Failed { child, .. } => child.as_ref(),
    }?;
    Some(ChildReport {
        code: child.code,
        signal: child.signal,
    })
}

fn child_failed(code: &str, task: &str, output: &ProcessOutput) -> CoreError {
    CoreError::new(
        if output.timed_out {
            ExitClass::Timeout
        } else {
            ExitClass::ChildFailed
        },
        if output.timed_out { "TIMEOUT" } else { code },
        format!("task `{task}` exited {}", output.mapped_exit()),
        "inspect the child output and task log, then retry",
    )
    .with_details(serde_json::json!({
        "child": {"code": output.child.code, "signal": output.child.signal}
    }))
}

fn resource_status(step: &wt_core::resource::StepResult) -> String {
    step.record
        .as_ref()
        .map(|record| format!("{:?}", record.state).to_ascii_lowercase())
        .unwrap_or_else(|| "dropped".to_owned())
}

fn scoped_id(key: &ResourceKey) -> String {
    if key.scope.as_str() == "." {
        key.task.clone()
    } else {
        format!("{}/{}", key.scope, key.task)
    }
}
