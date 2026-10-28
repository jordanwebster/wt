use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wt_core::config::{self, Arena, Command, Config, EffectiveScope, TiedTo};
use wt_core::env::{EnvInputs, EnvOutput, RecipeContext};
use wt_core::lifecycle::{ArenaHolder, RepoState};
use wt_core::model::{duration_millis, scope_enc};
use wt_core::recipe_plan::{HeldLocks, Node, Origin, PlannedLock, ResourcePlan};
use wt_core::report::{ChildReport, EnsureData, StepReport};
use wt_core::report::{Notice, NoticeLevel};
use wt_core::resource::{
    Action as ResourceDecision, EffectResult, Event, ExpandedCommand, Probe, ProbeResult,
    ResourceKey, ResourceRecord, ResourceSnapshot, SnapshotRoots,
};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, GitToken, ResourceToken};
use wt_sys::proc::{self, CommandRequest, ProcessOutput, Tee};
use wt_sys::snapshot::{Action as SnapshotAction, ExecuteResult, ExecutionOptions};

use super::context::duration;
use super::door::Door;
use super::Context;

enum Guard {
    Git { _token: GitToken },
    Resource { _token: ResourceToken },
}

pub(crate) struct PlanExecution {
    pub data: EnsureData,
    pub notices: Vec<Notice>,
}

pub(crate) struct ArenaSnapshot {
    repo: BTreeMap<String, ArenaHolder>,
    machine: BTreeMap<String, ArenaHolder>,
}

pub(crate) struct DestroyResult {
    pub state: String,
    pub child: Option<ChildReport>,
    pub notice: Option<Notice>,
}

pub(crate) struct ExecuteOptions<'a> {
    pub timeout: Option<&'a str>,
    pub wait: Option<&'a str>,
    pub no_log: bool,
    pub take: bool,
}

impl ExecuteOptions<'_> {
    pub const DEFAULT: Self = Self {
        timeout: None,
        wait: None,
        no_log: false,
        take: false,
    };
}

struct NodeExecution {
    status: String,
    child: Option<ChildReport>,
    log: Option<String>,
    notices: Vec<Notice>,
}

pub(crate) fn plan(context: &Context, door: &Door, root: &str) -> Result<ResourcePlan, CoreError> {
    let mut catalog = context.resource_catalog(&door.tree, &door.config)?;
    for node in catalog.values_mut().filter(|node| node.destroy.is_some()) {
        let tied_to = node.tied_to.ok_or_else(|| {
            CoreError::new(
                ExitClass::State,
                "CONFIG_INVALID",
                format!("resource {} has no tied_to", node.id),
                "set tied_to to tree, repo, or machine",
            )
        })?;
        node.resource = Some(ResourceKey {
            label: (tied_to != TiedTo::Machine).then(|| door.tree.label.clone()),
            tied_to,
            name: (tied_to == TiedTo::Tree).then(|| door.tree.name.clone()),
            scope: node.scope.clone(),
            resource: node.id.clone(),
        });
    }
    wt_core::recipe_plan::plan(root, &catalog)
}

pub(crate) fn execute_plan(
    context: &mut Context,
    door: &Door,
    plan: &ResourcePlan,
    options: ExecuteOptions<'_>,
) -> Result<PlanExecution, CoreError> {
    let mut steps = Vec::new();
    let mut notices = Vec::new();
    let mut last_child = None;
    let mut last_log = None;
    let root = plan
        .nodes
        .iter()
        .find(|node| node.id == plan.root)
        .expect("a resource plan contains its root");
    let displaced = if options.take {
        displace_arena(context, door, root)?
    } else {
        None
    };
    for (index, node) in plan.nodes.iter().enumerate() {
        let started = Instant::now();
        let _guards = acquire_node_locks(context, door, node, options.wait)?;
        let (status, child, log) = if node.resource.is_some() {
            refresh_node_declaration(context, door, node)?;
            let result = ensure_resource(context, door, node, options.timeout, options.no_log)?;
            notices.extend(result.notices);
            (result.status, result.child, result.log)
        } else {
            run_recipe(context, door, node, options.timeout, options.no_log)?
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
        data: EnsureData {
            target: door.target.to_string(),
            resource: plan.root.clone(),
            child: last_child,
            log: last_log,
            displaced,
            steps,
        },
        notices,
    })
}

pub(crate) fn refresh_all_declarations(
    context: &mut Context,
    door: &Door,
) -> Result<(), CoreError> {
    refresh_declarations(context, door, true)
}

pub(crate) fn refresh_lifecycle_declarations(
    context: &mut Context,
    door: &Door,
) -> Result<(), CoreError> {
    refresh_declarations(context, door, false)
}

fn refresh_declarations(
    context: &mut Context,
    door: &Door,
    include_machine: bool,
) -> Result<(), CoreError> {
    let mut catalog = context.resource_catalog(&door.tree, &door.config)?;
    for node in catalog.values_mut().filter(|node| {
        node.destroy.is_some() && (include_machine || node.tied_to != Some(TiedTo::Machine))
    }) {
        let tied_to = node.tied_to.unwrap_or(TiedTo::Tree);
        node.resource = Some(ResourceKey {
            label: (tied_to != TiedTo::Machine).then(|| door.tree.label.clone()),
            tied_to,
            name: (tied_to == TiedTo::Tree).then(|| door.tree.name.clone()),
            scope: node.scope.clone(),
            resource: node.id.clone(),
        });
        refresh_node_declaration(context, door, node)?;
    }
    mark_undeclared(context, door, &catalog, include_machine)?;
    Ok(())
}

fn mark_undeclared(
    context: &Context,
    door: &Door,
    catalog: &BTreeMap<String, Node>,
    include_machine: bool,
) -> Result<(), CoreError> {
    let declared = catalog
        .values()
        .filter_map(|node| node.resource.as_ref())
        .map(scoped_id)
        .collect::<BTreeSet<_>>();
    let holder = context.holder(door.target.to_string(), "refresh")?;
    context.mutate_state(&door.target, &holder, |state| {
        for (id, record) in &mut state.resources {
            if record.key.tied_to == TiedTo::Tree {
                record.undeclared = !declared.contains(id);
            }
        }
        Ok(())
    })?;

    let repo_key = ResourceKey {
        label: Some(door.tree.label.clone()),
        tied_to: TiedTo::Repo,
        name: None,
        scope: wt_core::model::RelDir::new(".")?,
        resource: String::new(),
    };
    mark_shared_undeclared(context, door, &repo_key, |id, _| !declared.contains(id))?;
    if include_machine {
        let machine_key = ResourceKey {
            label: None,
            tied_to: TiedTo::Machine,
            name: None,
            scope: wt_core::model::RelDir::new(".")?,
            resource: String::new(),
        };
        mark_shared_undeclared(context, door, &machine_key, |_, record| {
            !machine_resource_declared(context, &record.key)
        })?;
    }
    Ok(())
}

fn mark_shared_undeclared(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
    mut absent: impl FnMut(&str, &ResourceRecord) -> bool,
) -> Result<(), CoreError> {
    let (path, lock_path, label) = shared_state_paths(context, key);
    let holder = context.holder(door.target.to_string(), "refresh")?;
    let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
    let Some(mut state) = wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")? else {
        return Ok(());
    };
    for (id, record) in &mut state.resources {
        record.undeclared = absent(id, record);
    }
    state.schema = 2;
    state.label = label;
    wt_sys::fsx::write_json(&path, &state)
}

fn machine_resource_declared(context: &Context, key: &ResourceKey) -> bool {
    let base = key.resource.split('#').next().unwrap_or(&key.resource);
    context.registry.trees.iter().any(|tree| {
        if !tree.canonical {
            return false;
        }
        context.load_config(tree).is_ok_and(|config| {
            wt_core::config::effective_scope(&config, key.scope.as_str()).is_ok_and(|scope| {
                scope
                    .resources
                    .get(base)
                    .is_some_and(|resource| resource.tied_to == Some(TiedTo::Machine))
            })
        })
    })
}

pub(crate) fn probe_all_resources(context: &Context, door: &Door) -> Result<(), CoreError> {
    let mut records = context
        .read_state(&door.target)?
        .map(|state| state.resources.into_values().collect::<Vec<_>>())
        .unwrap_or_default();
    records.extend(
        wt_sys::fsx::read_json::<RepoState>(
            &context
                .home
                .join(wt_core::model::repo_state_path(&door.target.label)),
            "STATE_CORRUPT",
        )?
        .into_iter()
        .flat_map(|state| state.resources.into_values()),
    );
    records.extend(
        wt_sys::fsx::read_json::<RepoState>(
            &context.home.join(wt_core::model::machine_state_path()),
            "STATE_CORRUPT",
        )?
        .into_iter()
        .flat_map(|state| state.resources.into_values()),
    );
    probe_resources(context, door, records).map(|_| ())
}

pub(crate) fn probe_tree_resources(context: &Context, door: &Door) -> Result<(), CoreError> {
    let records = context
        .read_state(&door.target)?
        .map(|state| {
            state
                .resources
                .into_values()
                .filter(|record| record.key.tied_to == TiedTo::Tree)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    probe_resources(context, door, records).map(|_| ())
}

pub(crate) fn probe_resources(
    context: &Context,
    door: &Door,
    records: Vec<ResourceRecord>,
) -> Result<Vec<ResourceRecord>, CoreError> {
    let mut probed = Vec::with_capacity(records.len());
    for record in records {
        let arena = record.declaration.arena;
        if let Some(arena) = arena {
            let current = arena_holder(context, &record.key, arena)?;
            if current.as_deref() != Some(door.target.to_string().as_str()) {
                probed.push(record);
                continue;
            }
        }
        let holder = context.holder(door.target.to_string(), "probe")?;
        let _resource = lock::resource(
            &resource_lock_path(context, &record.key),
            &holder,
            duration(
                context.settings.waits.resource.as_deref(),
                Duration::from_secs(120),
            ),
        )?;
        let probe = execute_probe(context, door, record.effective_snapshot())?;
        let absent = matches!(probe.result, ProbeResult::Absent);
        let step = wt_core::resource::step(Some(record), Event::Probe(probe));
        let key = step
            .record
            .as_ref()
            .expect("probe keeps record")
            .key
            .clone();
        persist_step(context, door, &key, &step.record)?;
        if absent {
            if let Some(arena) = arena {
                clear_arena_holder(context, &key, arena, &door.target.to_string())?;
            }
        }
        probed.push(step.record.expect("probe keeps record"));
    }
    Ok(probed)
}

fn acquire_node_locks(
    context: &Context,
    door: &Door,
    node: &Node,
    wait: Option<&str>,
) -> Result<Vec<Guard>, CoreError> {
    let holder = context.holder(door.target.to_string(), "run")?;
    let mut guards = Vec::new();
    let plan = wt_core::recipe_plan::lock_plan(
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
                        wait.or(context.settings.waits.repo_git.as_deref()),
                        Duration::from_secs(60),
                    ),
                )?,
            }),
            PlannedLock::Resource { key } => guards.push(Guard::Resource {
                _token: lock::resource(
                    &resource_lock_path(context, &key),
                    &holder,
                    duration(
                        wait.or(context.settings.waits.resource.as_deref()),
                        Duration::from_secs(120),
                    ),
                )?,
            }),
        }
    }
    Ok(guards)
}

pub(crate) fn resource_lock_path(context: &Context, key: &ResourceKey) -> PathBuf {
    match key.tied_to {
        TiedTo::Tree => context.home.join(format!(
            "locks/{}/res/tree/{}/{}/{}.lock",
            key.label
                .as_ref()
                .expect("tree-tied resource keys carry a label"),
            key.name.as_deref().unwrap_or("canonical"),
            scope_enc(&key.scope),
            key.resource
        )),
        TiedTo::Repo => context.home.join(format!(
            "locks/{}/res/repo/{}/{}.lock",
            key.label
                .as_ref()
                .expect("repo-tied resource keys carry a label"),
            scope_enc(&key.scope),
            key.resource
        )),
        TiedTo::Machine => context.home.join(format!(
            "locks/_machine/res/{}/{}.lock",
            scope_enc(&key.scope),
            key.resource
        )),
    }
}

fn run_recipe(
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
            format!("recipe cwd {} is missing", cwd.display()),
            "create the directory or correct the recipe cwd",
        ));
    }
    if let Some(exists) = &node.exists {
        let request = request(exists, &assembled, &cwd, node.template)?;
        let probe = proc::probe(
            &request,
            duration(
                node.timeout
                    .as_deref()
                    .or(context.settings.recipes.probe_timeout.as_deref()),
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
                    "RECIPE_PROBE_FAILED",
                    format!("probe for {} failed", node.id),
                    "fix the probe and retry",
                ))
            }
        }
    }
    let Some(run) = &node.command else {
        return Ok(("skipped".to_owned(), None, None));
    };
    let log = log_path(context, door, node, no_log)?;
    let output = proc::run(
        &request(run, &assembled, &cwd, node.template)?,
        log.as_deref(),
        timeout
            .or(node.timeout.as_deref())
            .or(context.settings.recipes.timeout.as_deref())
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
        return Err(child_failed("RECIPE_FAILED", &node.id, &output));
    }
    Ok((
        "ok".to_owned(),
        Some(child),
        log.map(|path| path.to_string_lossy().into_owned()),
    ))
}

pub(crate) fn run_hook(
    context: &Context,
    door: &Door,
    name: &str,
    command: &Command,
    log: &Path,
) -> Result<ChildReport, CoreError> {
    let mut node = Node::from_recipe(
        name.to_owned(),
        wt_core::model::RelDir::new(".")?,
        Origin::Repo,
        &wt_core::config::Recipe {
            command: Some(command.clone()),
            ..wt_core::config::Recipe::default()
        },
    );
    node.cwd = wt_core::model::RelDir::new(".")?;
    let assembled = node_environment(context, door, &node)?;
    let request = request(
        command,
        &assembled,
        Path::new(door.tree.path.as_str()),
        true,
    )?;
    let output = proc::run(
        &request,
        Some(log),
        context
            .settings
            .recipes
            .timeout
            .as_deref()
            .and_then(duration_millis)
            .map(Duration::from_millis),
        Tee::Inherit,
    )?;
    let child = child_report(&output);
    if !output.success() {
        return Err(child_failed("HOOK_FAILED", name, &output));
    }
    Ok(child)
}

pub(crate) fn acquire_instance(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
    holder: &wt_sys::lock::Holder,
) -> Result<String, CoreError> {
    let mut record = load_record(context, door, key)?;
    if let Some(command) = record.effective_snapshot().acquire.clone() {
        let effect =
            match execute_effect_command(context, door, record.effective_snapshot(), &command) {
                Ok(effect) => effect,
                Err(error) => {
                    record.hold = wt_core::resource::ResourceHold::Free;
                    persist_step(context, door, key, &Some(record))?;
                    return Err(error);
                }
            };
        if let EffectResult::Failed { message, .. } = effect {
            record.hold = wt_core::resource::ResourceHold::Free;
            persist_step(context, door, key, &Some(record))?;
            return Err(CoreError::new(
                ExitClass::ChildFailed,
                "ACQUIRE_FAILED",
                message,
                "inspect the acquire recipe and retry",
            ));
        }
    }
    let name = record.effective_snapshot().name.clone();
    record.hold = wt_core::resource::ResourceHold::Leased {
        pid: holder.pid,
        process_start: holder.process_start.clone(),
    };
    persist_step(context, door, key, &Some(record))?;
    Ok(name)
}

pub(crate) fn release_instance(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
    hold: wt_core::resource::ResourceHold,
) -> Result<(), CoreError> {
    let mut record = load_record(context, door, key)?;
    record.released_at = Some(wt_sys::fsx::timestamp()?);
    record.released_millis = Some(now_millis());
    record.hold = hold;
    persist_step(context, door, key, &Some(record))
}

pub(crate) fn set_instance_hold(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
    hold: wt_core::resource::ResourceHold,
) -> Result<(), CoreError> {
    let mut record = load_record(context, door, key)?;
    record.hold = hold;
    persist_step(context, door, key, &Some(record))
}

pub(crate) fn clear_stored_keep(
    context: &Context,
    target: &wt_core::model::Target,
    key: &ResourceKey,
) -> Result<(), CoreError> {
    let record = match key.tied_to {
        TiedTo::Tree => context
            .read_state(target)?
            .and_then(|state| state.resources.get(&scoped_id(key)).cloned()),
        TiedTo::Repo | TiedTo::Machine => {
            let (path, _, _) = shared_state_paths(context, key);
            wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?
                .and_then(|state| state.resources.get(&scoped_id(key)).cloned())
        }
    };
    let Some(mut record) = record else {
        return Ok(());
    };
    if matches!(record.hold, wt_core::resource::ResourceHold::Kept { .. }) {
        record.hold = wt_core::resource::ResourceHold::Free;
        persist_tree_record(context, target, key, &Some(record))?;
    }
    Ok(())
}

fn execute_effect_command(
    context: &Context,
    door: &Door,
    snapshot: &ResourceSnapshot,
    command: &wt_core::resource::ExpandedCommand,
) -> Result<EffectResult, CoreError> {
    let mut temporary = snapshot.clone();
    temporary.acquire = Some(command.clone());
    execute_effect(context, door, &temporary, SnapshotAction::Acquire, None)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn ensure_resource(
    context: &Context,
    door: &Door,
    node: &Node,
    _timeout: Option<&str>,
    no_log: bool,
) -> Result<NodeExecution, CoreError> {
    let key = node.resource.as_ref().expect("resource node has a key");
    let mut record = load_record(context, door, key)?;
    let arena = node.arena;
    let prior_holder = if let Some(arena) = arena {
        arena_holders(context, key, arena)?
            .into_iter()
            .find(|holder| holder == &door.target.to_string())
    } else {
        None
    };
    let snapshot = record.effective_snapshot().clone();
    let probe = execute_probe(context, door, &snapshot)?;
    if let Some(arena) = arena {
        if !matches!(probe.result, ProbeResult::Failed { .. }) {
            if prior_holder.is_some() && matches!(probe.result, ProbeResult::Absent) {
                clear_arena_holder(context, key, arena, &door.target.to_string())?;
            }
            claim_arena(context, door, node, arena)?;
        }
    }
    let first = wt_core::resource::step(
        Some(record),
        Event::Create {
            probe: probe.clone(),
            create: None,
            confirm: None,
        },
    );
    persist_step(context, door, key, &first.record)?;
    if let Some(error) = first.error {
        return Err(error);
    }
    if first.action != ResourceDecision::Create {
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
        SnapshotAction::Create,
        log.as_deref(),
    )?;
    let child = effect_child(&effect);
    let confirm = execute_probe(context, door, record.effective_snapshot())?;
    let confirmed_absent = matches!(confirm.result, ProbeResult::Absent);
    let final_step = wt_core::resource::step(
        Some(record),
        Event::Create {
            probe,
            create: Some(effect),
            confirm: Some(confirm),
        },
    );
    persist_step(context, door, key, &final_step.record)?;
    if confirmed_absent {
        if let Some(arena) = arena {
            clear_arena_holder(context, key, arena, &door.target.to_string())?;
        }
    }
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
        })
        .collect()
}

pub(crate) fn destroy_resource(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
    teardown: bool,
    arenas: &ArenaSnapshot,
) -> Result<DestroyResult, CoreError> {
    let mut record = load_record(context, door, key)?;
    let arena = record.declaration.arena;
    if let Some(holder) =
        live_other_arena_holder(context, arenas, &record, &door.target.to_string())
    {
        if teardown {
            persist_step(context, door, key, &None)?;
            return Ok(DestroyResult {
                state: "dropped".to_owned(),
                child: None,
                notice: Some(resource_held_by_notice(key, &holder)),
            });
        }
        return Ok(DestroyResult {
            state: "declared".to_owned(),
            child: None,
            notice: Some(resource_held_by_notice(key, &holder)),
        });
    }
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
        if let Some(arena) = arena {
            clear_arena_holder(context, key, arena, &door.target.to_string())?;
        }
        return Ok(DestroyResult {
            state: resource_status(&first),
            child: None,
            notice: None,
        });
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
    if let Some(arena) = arena {
        clear_arena_holder(context, key, arena, &door.target.to_string())?;
    }
    Ok(DestroyResult {
        state: resource_status(&final_step),
        child,
        notice: None,
    })
}

pub(crate) fn destroy_stored_resource(
    context: &Context,
    target: &wt_core::model::Target,
    record: ResourceRecord,
    tree_replaced: bool,
    arenas: &ArenaSnapshot,
) -> Result<DestroyResult, CoreError> {
    let key = record.key.clone();
    let arena = record.declaration.arena;
    if let Some(holder) = live_other_arena_holder(context, arenas, &record, &target.to_string()) {
        persist_tree_record(context, target, &key, &None)?;
        return Ok(DestroyResult {
            state: "dropped".to_owned(),
            child: None,
            notice: Some(resource_held_by_notice(&key, &holder)),
        });
    }
    // An orphan is asked again before it is destroyed again: its last destroy
    // may have taken it away while reporting failure, and a destroy that
    // refuses what is already gone would then keep it orphaned for good. Only
    // when the probe still cannot answer is it destroyed as if present.
    let probe = match record.state {
        wt_core::resource::ResourceState::Present => Probe::present(wt_sys::fsx::timestamp()?),
        wt_core::resource::ResourceState::Orphaned => {
            let probe = execute_stored_probe(context, &record, tree_replaced)?;
            if matches!(probe.result, wt_core::resource::ProbeResult::Failed { .. }) {
                Probe::present(wt_sys::fsx::timestamp()?)
            } else {
                probe
            }
        }
        _ => execute_stored_probe(context, &record, tree_replaced)?,
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
        if let Some(arena) = arena {
            clear_arena_holder(context, &key, arena, &target.to_string())?;
        }
        return Ok(DestroyResult {
            state: resource_status(&first),
            child: None,
            notice: None,
        });
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
    if let Some(arena) = arena {
        clear_arena_holder(context, &key, arena, &target.to_string())?;
    }
    Ok(DestroyResult {
        state: resource_status(&final_step),
        child,
        notice: None,
    })
}

pub(crate) fn newest_resources_first(mut records: Vec<ResourceRecord>) -> Vec<ResourceRecord> {
    records.sort_by(|left, right| {
        right
            .effective_snapshot()
            .recorded_sequence
            .cmp(&left.effective_snapshot().recorded_sequence)
            .then_with(|| {
                right
                    .effective_snapshot()
                    .recorded_at
                    .cmp(&left.effective_snapshot().recorded_at)
            })
            .then_with(|| scoped_id(&right.key).cmp(&scoped_id(&left.key)))
    });
    records
}

/// The registered checkout of the repository a stored record belongs to. A
/// machine record's key names no repository, so its repository is the one
/// whose tree recorded the snapshot, found among the live trees or, once that
/// tree is removed, its tombstone: the recipes are that repository's scripts,
/// and from the wt home a recipe naming them by relative path cannot start.
/// Only when no registered repository owned that tree does it get the wt home.
/// The snapshot's own rules decide what that means for the recipe's directory.
fn stored_repo_root(context: &Context, record: &ResourceRecord) -> PathBuf {
    let tree = record.effective_snapshot().roots.tree.as_str();
    let label = record.key.label.as_ref().or_else(|| {
        let registry = &context.registry;
        registry
            .trees
            .iter()
            .find(|candidate| candidate.path.as_str() == tree)
            .map(|candidate| &candidate.label)
            .or_else(|| {
                registry
                    .tombstones
                    .iter()
                    .rev()
                    .find(|candidate| candidate.path.as_str() == tree)
                    .map(|candidate| &candidate.label)
            })
    });
    label
        .and_then(|label| context.registry.labels.get(label))
        .map(|repo| PathBuf::from(repo.path.as_str()))
        .unwrap_or_else(|| context.home.clone())
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
        &stored_repo_root(context, record),
        ExecutionOptions {
            tree_replaced,
            deadlines: wt_sys::snapshot::Deadlines::from_settings(&context.settings.recipes)?,
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
        &stored_repo_root(context, record),
        ExecutionOptions {
            tree_replaced,
            deadlines: wt_sys::snapshot::Deadlines::from_settings(&context.settings.recipes)?,
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

/// Frees every shared record still shown as kept by `target`. A keep file is
/// what reserves a slot, and an expired one is deleted when next read, but
/// the record's copy of the hold is written only on release, so a keep that
/// lapsed before its tree was removed would otherwise name that tree forever.
pub(crate) fn clear_shared_keeps_of(
    context: &Context,
    target: &wt_core::model::Target,
) -> Result<(), CoreError> {
    let holder = context.holder(target.to_string(), "resource")?;
    let tree = target.to_string();
    let stores = [
        (
            context
                .home
                .join(wt_core::model::repo_state_path(&target.label)),
            context
                .home
                .join(format!("locks/{}/_repo.rmw.lock", target.label)),
        ),
        (
            context.home.join(wt_core::model::machine_state_path()),
            context.home.join("locks/_machine.rmw.lock"),
        ),
    ];
    for (path, lock_path) in stores {
        let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
        let Some(mut state) = wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")? else {
            continue;
        };
        let mut changed = false;
        for record in state.resources.values_mut() {
            if matches!(&record.hold, wt_core::resource::ResourceHold::Kept { tree: kept, .. } if *kept == tree)
            {
                record.hold = wt_core::resource::ResourceHold::Free;
                changed = true;
            }
        }
        if changed {
            wt_sys::fsx::write_json(&path, &state)?;
        }
    }
    Ok(())
}

fn persist_tree_record(
    context: &Context,
    target: &wt_core::model::Target,
    key: &ResourceKey,
    record: &Option<ResourceRecord>,
) -> Result<(), CoreError> {
    let holder = context.holder(target.to_string(), "resource")?;
    let id = scoped_id(key);
    match key.tied_to {
        TiedTo::Tree => context.mutate_state(target, &holder, |state| {
            if let Some(record) = record {
                state.resources.insert(id, record.clone());
            } else {
                state.resources.remove(&id);
            }
            Ok(())
        }),
        TiedTo::Repo | TiedTo::Machine => {
            let (path, lock_path, label) = shared_state_paths(context, key);
            let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
            let mut state =
                wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?.unwrap_or(RepoState {
                    schema: 2,
                    label,
                    resources: BTreeMap::new(),
                    arenas: BTreeMap::new(),
                });
            if let Some(record) = record {
                state.resources.insert(id, record.clone());
            } else {
                state.resources.remove(&id);
            }
            wt_sys::fsx::write_json(&path, &state)
        }
    }
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
            deadlines: wt_sys::snapshot::Deadlines::from_settings(&context.settings.recipes)?,
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
            deadlines: wt_sys::snapshot::Deadlines::from_settings(&context.settings.recipes)?,
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
                state.last_error = Some(format!("REFRESH_SKIPPED:{}:{}", key.scope, key.resource));
                Ok(())
            })?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let effective = config_for_node(&door.config, node)?;
    let bin_dirs = if key.tied_to == TiedTo::Machine {
        Vec::new()
    } else {
        effective
            .bin
            .iter()
            .map(|path| Path::new(door.tree.path.as_str()).join(path.as_str()))
            .collect::<Vec<_>>()
    };
    let bin_exes = wt_sys::snapshot::capture_bin_exes(&bin_dirs)?;
    let snapshot = ResourceSnapshot {
        schema: 2,
        key: key.clone(),
        arena: node.arena,
        capacity: node.capacity,
        name: assembled
            .env
            .get("WT_SELF")
            .cloned()
            .expect("resource assembly always sets WT_SELF"),
        cwd_rel: node.cwd.clone(),
        exists: node
            .exists
            .as_ref()
            .map(|command| expanded(command, &assembled, node.template))
            .transpose()?,
        destroy: expanded(
            node.destroy.as_ref().expect("resource has destroy"),
            &assembled,
            node.template,
        )?,
        create: node
            .command
            .as_ref()
            .map(|command| expanded(command, &assembled, node.template))
            .transpose()?,
        acquire: effective
            .resources
            .get(node.id.split('#').next().unwrap_or(&node.id))
            .and_then(|resource| resource.acquire.as_ref())
            .map(|command| expanded(command, &assembled, node.template))
            .transpose()?,
        keep: effective
            .resources
            .get(node.id.split('#').next().unwrap_or(&node.id))
            .and_then(|resource| resource.keep.clone()),
        idle: effective
            .resources
            .get(node.id.split('#').next().unwrap_or(&node.id))
            .and_then(|resource| resource.idle.clone()),
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
        recorded_sequence: 0,
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
            snapshot.recorded_sequence = next_recorded_sequence(state.resources.values());
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
        TiedTo::Repo | TiedTo::Machine => {
            wt_core::declarations::strip_tree_specific_env(&mut snapshot);
            let (path, lock_path, label) = shared_state_paths(context, key);
            let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
            let mut state =
                wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?.unwrap_or(RepoState {
                    schema: 2,
                    label,
                    resources: BTreeMap::new(),
                    arenas: BTreeMap::new(),
                });
            snapshot.recorded_sequence = next_recorded_sequence(state.resources.values());
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

fn next_recorded_sequence<'a>(records: impl Iterator<Item = &'a ResourceRecord>) -> u64 {
    records
        .flat_map(|record| std::iter::once(&record.declaration).chain(record.instance.iter()))
        .map(|snapshot| snapshot.recorded_sequence)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

pub(crate) fn load_record(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
) -> Result<ResourceRecord, CoreError> {
    let record = match key.tied_to {
        TiedTo::Tree => context
            .read_state(&door.target)?
            .and_then(|state| state.resources.get(&scoped_id(key)).cloned()),
        TiedTo::Repo | TiedTo::Machine => {
            let (path, _, _) = shared_state_paths(context, key);
            wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?
                .and_then(|state| state.resources.get(&scoped_id(key)).cloned())
        }
    };
    record.ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "RESOURCE_RECORD_MISSING",
            format!("resource {} has no declaration record", key.resource),
            "run `wt with` again to refresh its declaration",
        )
    })
}

pub(crate) fn persist_step(
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
        TiedTo::Repo | TiedTo::Machine => {
            let (path, lock_path, label) = shared_state_paths(context, key);
            let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
            let mut state =
                wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?.unwrap_or(RepoState {
                    schema: 2,
                    label,
                    resources: BTreeMap::new(),
                    arenas: BTreeMap::new(),
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

fn shared_state_paths(
    context: &Context,
    key: &ResourceKey,
) -> (PathBuf, PathBuf, Option<wt_core::model::Label>) {
    match key.tied_to {
        TiedTo::Repo => {
            let label = key
                .label
                .as_ref()
                .expect("repo-tied resource keys carry a label")
                .clone();
            (
                context.home.join(wt_core::model::repo_state_path(&label)),
                context.home.join(format!("locks/{label}/_repo.rmw.lock")),
                Some(label),
            )
        }
        TiedTo::Machine => (
            context.home.join(wt_core::model::machine_state_path()),
            context.home.join("locks/_machine.rmw.lock"),
            None,
        ),
        TiedTo::Tree => unreachable!("tree state uses the tree store"),
    }
}

fn arena_state_paths(
    context: &Context,
    key: &ResourceKey,
    arena: Arena,
) -> (PathBuf, PathBuf, Option<wt_core::model::Label>) {
    match arena {
        Arena::Repo => {
            let label = key
                .label
                .as_ref()
                .expect("repository arenas carry a label")
                .clone();
            (
                context.home.join(wt_core::model::repo_state_path(&label)),
                context.home.join(format!("locks/{label}/_repo.rmw.lock")),
                Some(label),
            )
        }
        Arena::Machine => (
            context.home.join(wt_core::model::machine_state_path()),
            context.home.join("locks/_machine.rmw.lock"),
            None,
        ),
    }
}

fn target_is_live(context: &Context, value: &str) -> bool {
    wt_core::model::Target::parse(value).is_ok_and(|target| {
        context
            .registry
            .trees
            .iter()
            .any(|tree| tree.label == target.label && tree.name == target.name)
    })
}

fn arena_holder(
    context: &Context,
    key: &ResourceKey,
    arena: Arena,
) -> Result<Option<String>, CoreError> {
    Ok(arena_holders(context, key, arena)?.into_iter().next())
}

fn arena_holders(
    context: &Context,
    key: &ResourceKey,
    arena: Arena,
) -> Result<Vec<String>, CoreError> {
    let (path, _, _) = arena_state_paths(context, key, arena);
    let base = scoped_id(key);
    let prefix = format!("{base}@");
    let mut holders = wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?
        .map(|state| {
            state
                .arenas
                .into_iter()
                .filter(|(id, _)| id == &base || id.starts_with(&prefix))
                .map(|(_, entry)| entry.holder)
                .filter(|holder| target_is_live(context, holder))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    holders.sort();
    holders.dedup();
    Ok(holders)
}

pub(crate) fn arena_snapshot(
    context: &Context,
    label: &wt_core::model::Label,
) -> Result<ArenaSnapshot, CoreError> {
    let repo = wt_sys::fsx::read_json::<RepoState>(
        &context.home.join(wt_core::model::repo_state_path(label)),
        "STATE_CORRUPT",
    )?
    .map(|state| state.arenas)
    .unwrap_or_default();
    let machine = wt_sys::fsx::read_json::<RepoState>(
        &context.home.join(wt_core::model::machine_state_path()),
        "STATE_CORRUPT",
    )?
    .map(|state| state.arenas)
    .unwrap_or_default();
    Ok(ArenaSnapshot { repo, machine })
}

fn live_other_arena_holder(
    context: &Context,
    arenas: &ArenaSnapshot,
    record: &ResourceRecord,
    target: &str,
) -> Option<String> {
    if record.key.tied_to != TiedTo::Tree {
        return None;
    }
    for arena in [Arena::Repo, Arena::Machine] {
        if record
            .declaration
            .arena
            .is_some_and(|declared| declared != arena)
        {
            continue;
        }
        let entries = match arena {
            Arena::Repo => &arenas.repo,
            Arena::Machine => &arenas.machine,
        };
        let base = scoped_id(&record.key);
        let prefix = format!("{base}@");
        let matches_resource = |slot: &String| slot == &base || slot.starts_with(&prefix);
        if entries
            .iter()
            .any(|(slot, entry)| matches_resource(slot) && entry.holder == target)
        {
            return None;
        }
        let Some(holder) = entries
            .iter()
            .find(|(slot, entry)| {
                matches_resource(slot)
                    && entry.holder != target
                    && target_is_live(context, &entry.holder)
            })
            .map(|(_, entry)| entry.holder.as_str())
        else {
            continue;
        };
        if record.declaration.arena.is_none()
            && !wt_core::model::Target::parse(holder)
                .is_ok_and(|holder_target| Some(&holder_target.label) == record.key.label.as_ref())
        {
            continue;
        }
        return Some(holder.to_owned());
    }
    None
}

fn resource_held_by_notice(key: &ResourceKey, holder: &str) -> Notice {
    Notice {
        level: NoticeLevel::Info,
        code: "RESOURCE_HELD_BY".to_owned(),
        subject: Some(scoped_id(key)),
        message: format!("resource left to {holder}"),
    }
}

pub(crate) fn arena_holder_for_record(
    context: &Context,
    record: &ResourceRecord,
) -> Result<Option<String>, CoreError> {
    record
        .declaration
        .arena
        .map(|arena| arena_holder(context, &record.key, arena))
        .transpose()
        .map(Option::flatten)
}

fn claim_arena(context: &Context, door: &Door, node: &Node, arena: Arena) -> Result<(), CoreError> {
    let key = node.resource.as_ref().expect("arena resource has a key");
    let (path, lock_path, label) = arena_state_paths(context, key, arena);
    let owner = door.target.to_string();
    let holder = context.holder(owner.clone(), "arena-claim")?;
    let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
    let mut state =
        wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")?.unwrap_or(RepoState {
            schema: 2,
            label,
            resources: BTreeMap::new(),
            arenas: BTreeMap::new(),
        });
    let base = scoped_id(key);
    let prefix = format!("{base}@");
    let capacity = node.capacity.unwrap_or(1);
    state.arenas.retain(|id, entry| {
        !(id == &base || id.starts_with(&prefix)) || target_is_live(context, &entry.holder)
    });
    if state
        .arenas
        .iter()
        .any(|(id, entry)| (id == &base || id.starts_with(&prefix)) && entry.holder == owner)
    {
        return Ok(());
    }
    let slot = (1..=capacity).find(|slot| !state.arenas.contains_key(&arena_slot(&base, *slot)));
    let Some(slot) = slot else {
        let current = state
            .arenas
            .iter()
            .find(|(id, _)| *id == &base || id.starts_with(&prefix))
            .map(|(_, entry)| entry.holder.clone())
            .expect("a full arena has a holder");
        return Err(resource_held(node, door, &current));
    };
    state.arenas.insert(
        arena_slot(&base, slot),
        ArenaHolder {
            holder: owner,
            since: wt_sys::fsx::timestamp()?,
        },
    );
    wt_sys::fsx::write_json(&path, &state)
}

fn clear_arena_holder(
    context: &Context,
    key: &ResourceKey,
    arena: Arena,
    expected: &str,
) -> Result<(), CoreError> {
    let (path, lock_path, _) = arena_state_paths(context, key, arena);
    let holder = context.holder(expected.to_owned(), "arena-clear")?;
    let _lock = lock::state_rmw(&lock_path, &holder, context.rmw_wait())?;
    let Some(mut state) = wt_sys::fsx::read_json::<RepoState>(&path, "STATE_CORRUPT")? else {
        return Ok(());
    };
    let id = scoped_id(key);
    let prefix = format!("{id}@");
    let before = state.arenas.len();
    state.arenas.retain(|slot, entry| {
        !((slot == &id || slot.starts_with(&prefix)) && entry.holder == expected)
    });
    if state.arenas.len() != before {
        wt_sys::fsx::write_json(&path, &state)?;
    }
    Ok(())
}

fn resource_held(node: &Node, door: &Door, holder: &str) -> CoreError {
    CoreError::new(
        ExitClass::Conflict,
        "RESOURCE_HELD",
        format!("resource arena for {} is held by {holder}", node.id),
        format!(
            "run `wt with {} --take` in {} to displace {holder}",
            node.id, door.target
        ),
    )
    .with_details(serde_json::json!({"holder": holder}))
}

fn arena_slot(base: &str, slot: u16) -> String {
    if slot == 1 {
        base.to_owned()
    } else {
        format!("{base}@{slot}")
    }
}

fn displace_arena(
    context: &Context,
    door: &Door,
    node: &Node,
) -> Result<Option<String>, CoreError> {
    let arena = node
        .arena
        .expect("--take validation requires an arena root");
    let key = node.resource.as_ref().expect("arena resource has a key");
    loop {
        let Some(holder) = arena_holder(context, key, arena)? else {
            return Ok(None);
        };
        if holder == door.target.to_string() {
            return Ok(None);
        }
        let holder_target =
            wt_core::model::Target::parse(&holder).expect("live arena holders are valid targets");
        let holder_key = ResourceKey {
            label: Some(holder_target.label.clone()),
            tied_to: TiedTo::Tree,
            name: Some(holder_target.name.clone()),
            scope: key.scope.clone(),
            resource: key.resource.clone(),
        };
        let lock_holder = context.holder(door.target.to_string(), "take")?;
        let _resource = lock::resource(
            &resource_lock_path(context, &holder_key),
            &lock_holder,
            duration(
                context.settings.waits.resource.as_deref(),
                Duration::from_secs(120),
            ),
        )?;
        if arena_holder(context, key, arena)?.as_deref() != Some(holder.as_str()) {
            continue;
        }
        let record = context
            .read_state(&holder_target)?
            .and_then(|state| state.resources.get(&scoped_id(&holder_key)).cloned());
        let Some(record) = record else {
            clear_arena_holder(context, key, arena, &holder)?;
            return Ok(Some(holder));
        };
        let tree = context
            .registry
            .trees
            .iter()
            .find(|tree| tree.label == holder_target.label && tree.name == holder_target.name)
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::Conflict,
                    "ARENA_HOLDER_CHANGED",
                    format!(
                        "resource arena for {} has an unregistered holder {holder}",
                        node.id
                    ),
                    "the holder changed while displacing; run the command again",
                )
                .with_details(serde_json::json!({"holder": holder}))
            })?;
        let exists = matches!(
            wt_sys::fsx::path_kind(Path::new(tree.path.as_str()))?,
            wt_sys::fsx::PathKind::Directory
        );
        let replaced = exists && !context.identity_ok(tree)?;
        let arenas = arena_snapshot(context, &holder_target.label)?;
        destroy_stored_resource(context, &holder_target, record, replaced, &arenas)?;
        return Ok(Some(holder));
    }
}

pub(crate) fn resource_state(
    context: &Context,
    door: &Door,
    key: &ResourceKey,
) -> Result<Option<String>, CoreError> {
    Ok(load_record(context, door, key)
        .ok()
        .map(|record| format!("{:?}", record.state).to_ascii_lowercase()))
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
            .unwrap_or_else(|| wt_core::resource::default_name_template(key))
    });
    wt_core::assemble(EnvInputs {
        cfg: &effective,
        tree: &door.identity,
        home: &context.home.to_string_lossy(),
        contributed: super::door::present_resource_env(context, &door.target)?,
        recipe: Some(RecipeContext {
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

fn expanded(
    command: &Command,
    assembled: &EnvOutput,
    template: bool,
) -> Result<ExpandedCommand, CoreError> {
    if !template {
        return Ok(command.clone().into());
    }
    let context = wt_core::template::Context {
        vars: &assembled.vars,
        functions: &assembled.functions,
    };
    match command {
        Command::Shell(shell) => Ok(ExpandedCommand::Shell {
            shell: wt_core::template::expand(shell, &context)?,
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
    template: bool,
) -> Result<CommandRequest, CoreError> {
    CommandRequest::expanded(
        &expanded(command, assembled, template)?,
        cwd,
        assembled.env.clone(),
    )
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

fn child_failed(code: &str, recipe: &str, output: &ProcessOutput) -> CoreError {
    CoreError::new(
        if output.timed_out {
            ExitClass::Timeout
        } else {
            ExitClass::ChildFailed
        },
        if output.timed_out { "TIMEOUT" } else { code },
        format!("recipe `{recipe}` exited {}", output.mapped_exit()),
        "inspect the child output and recipe log, then retry",
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

/// Names the tree-tied resources this target must tear down before wt can stop
/// tracking it. A record without an instance is a declaration and nothing more:
/// the configuration mentions the resource, but nothing was ever created, so
/// there is nothing to orphan. A record whose arena arena is held by another
/// live tree belongs to that tree, which is the same test `destroy` applies
/// before it declines to touch one — refusing on a record `destroy` will not
/// clear would leave no way forward.
pub(crate) fn instantiated_resources(
    context: &Context,
    label: &wt_core::model::Label,
    target: &wt_core::model::Target,
) -> Result<Vec<String>, CoreError> {
    let Some(state) = context.read_state(target)? else {
        return Ok(Vec::new());
    };
    let arenas = arena_snapshot(context, label)?;
    let address = target.to_string();
    Ok(state
        .resources
        .values()
        .filter(|record| record.instance.is_some())
        .filter(|record| live_other_arena_holder(context, &arenas, record, &address).is_none())
        .map(|record| scoped_id(&record.key))
        .collect())
}

fn scoped_id(key: &ResourceKey) -> String {
    if key.scope.as_str() == "." {
        key.resource.clone()
    } else {
        format!("{}/{}", key.scope, key.resource)
    }
}
