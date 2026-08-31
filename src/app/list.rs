use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use wt_core::config::TiedTo;
use wt_core::lifecycle::{DerivedPhase, RepoState};
use wt_core::model::{Label, TreeRec};
use wt_core::report::{
    DirtyReport, LastErrorReport, LastProbeReport, ListData, PortReport, ResourceReport,
    SyncTreeReport, TreeReport, UpstreamReport, VerifyTreeReport,
};
use wt_core::resource::{ProbeResult, ResourceKey, ResourceRecord, ResourceState};
use wt_core::CoreError;

use crate::cli::List;

use super::context::target_of;
use super::{door, executor, Context, Output};

pub(crate) fn run(context: &mut Context, args: List) -> Result<Output, CoreError> {
    let selected = context
        .registry
        .trees
        .clone()
        .into_iter()
        .filter(|tree| {
            args.label
                .as_deref()
                .is_none_or(|label| tree.label.as_str() == label)
        })
        .collect::<Vec<_>>();
    let shared = if args.probe {
        probe_list_resources(context, &selected)?
    } else {
        SharedResourceRecords::read(context, &selected)?
    };
    let mut trees = Vec::new();
    for tree in selected {
        trees.push(tree_report_with_shared(
            context,
            &tree,
            args.fast,
            args.disk,
            false,
            Some(&shared),
        )?);
    }
    wt_core::report::sort_trees(&mut trees);
    let mut locks = Vec::new();
    for tree in &context.registry.trees {
        if args
            .label
            .as_deref()
            .is_some_and(|label| tree.label.as_str() != label)
        {
            continue;
        }
        let target = target_of(tree);
        for holder in super::remove::door_holders(context, &target)? {
            locks.push(wt_core::report::ListLockReport {
                name: target.to_string(),
                label: tree.label.to_string(),
                holder: wt_core::report::HolderReport {
                    pid: holder.pid,
                    target: target.to_string(),
                    verb: holder.verb,
                    since: holder.since,
                },
            });
        }
    }
    locks.sort_by(|left, right| {
        (&left.label, &left.name, left.holder.pid).cmp(&(
            &right.label,
            &right.name,
            right.holder.pid,
        ))
    });
    Output::data(ListData { trees, locks })
}

pub(crate) fn tree_report(
    context: &mut Context,
    tree: &wt_core::model::TreeRec,
    fast: bool,
    disk: bool,
    probe: bool,
) -> Result<TreeReport, CoreError> {
    tree_report_with_shared(context, tree, fast, disk, probe, None)
}

fn tree_report_with_shared(
    context: &mut Context,
    tree: &wt_core::model::TreeRec,
    fast: bool,
    disk: bool,
    probe: bool,
    shared: Option<&SharedResourceRecords>,
) -> Result<TreeReport, CoreError> {
    let target = target_of(tree);
    let exists = matches!(
        wt_sys::fsx::path_kind(Path::new(tree.path.as_str()))?,
        wt_sys::fsx::PathKind::Directory
    );
    let identity_ok = exists && context.identity_ok(tree)?;
    if probe && identity_ok {
        let mut entered = door::enter(context, Some(&target.to_string()), "probe")?;
        executor::refresh_all_declarations(context, &entered)?;
        executor::probe_all_resources(context, &entered)?;
        entered.release_gate();
    }
    let state = context.read_state(&target)?;
    let phase = context.phase(tree, state.as_ref())?;
    let (branch, detached_sha, dirty, upstream, behind_default, drift) = if exists && identity_ok {
        let git = context.git(Path::new(tree.path.as_str()))?;
        let branch = git.head_branch(Path::new(tree.path.as_str()))?;
        let status = git.status_porcelain(Path::new(tree.path.as_str()))?;
        let modified = status
            .iter()
            .filter(|entry| entry.index != '?' && entry.worktree != '?')
            .count() as u64;
        let untracked = status.iter().filter(|entry| entry.index == '?').count() as u64;
        let detached_sha = if branch.is_none() {
            Some(git.head_oid()?)
        } else {
            None
        };
        let upstream = if let Some(branch) = branch.as_deref() {
            if git.upstream_track(branch)? == "[gone]" {
                None
            } else {
                git.upstream(branch)?
                    .map(|upstream| git.ahead_behind("HEAD", &upstream))
                    .transpose()?
            }
        } else {
            None
        };
        let default = git.default_branch()?;
        let default_ref = format!("refs/remotes/origin/{default}");
        let behind_default = if git.ref_exists(&default_ref)? {
            Some(to_u32(git.ahead_behind("HEAD", &default_ref)?.behind))
        } else {
            None
        };
        let drift = if fast || !git.ref_exists(&default_ref)? {
            Vec::new()
        } else {
            let inputs = state
                .as_ref()
                .and_then(|state| state.sync.as_ref())
                .map(|sync| sync.inputs.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            if inputs.is_empty() {
                Vec::new()
            } else {
                let names = git
                    .diff_name_only("HEAD", &default_ref, &[])?
                    .into_iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                wt_core::drift::drift(&names, &inputs)
            }
        };
        (
            branch,
            detached_sha,
            Some(DirtyReport {
                modified,
                untracked,
            }),
            upstream.map(|value| UpstreamReport {
                ahead: to_u32(value.ahead),
                behind: to_u32(value.behind),
            }),
            behind_default,
            drift,
        )
    } else {
        (None, None, None, None, None, Vec::new())
    };
    let session = session_state(context, tree);
    let mut ports = tree
        .ports
        .iter()
        .map(|(name, index)| PortReport {
            name: name.to_string(),
            port: tree.geometry.port_base + u16::from(*index),
            bound: if fast {
                None
            } else {
                wt_sys::net::squat_probe(
                    tree.geometry.port_base + u16::from(*index),
                    Duration::from_millis(50),
                )
                .ok()
            },
        })
        .collect::<Vec<_>>();
    ports.sort_by_key(|port| {
        tree.ports[&wt_core::model::PortName::new(&port.name).expect("stored port name is valid")]
    });
    let mut resource_records = state
        .as_ref()
        .into_iter()
        .flat_map(|state| state.resources.values().cloned())
        .collect::<Vec<_>>();
    if let Some(shared) = shared {
        resource_records.extend(shared.for_tree(tree));
    } else {
        resource_records.extend(
            wt_sys::fsx::read_json::<RepoState>(
                &context
                    .home
                    .join(wt_core::model::repo_state_path(&tree.label)),
                "STATE_CORRUPT",
            )?
            .into_iter()
            .flat_map(|state| state.resources.into_values()),
        );
        resource_records.extend(
            wt_sys::fsx::read_json::<RepoState>(
                &context.home.join(wt_core::model::machine_state_path()),
                "STATE_CORRUPT",
            )?
            .into_iter()
            .flat_map(|state| state.resources.into_values()),
        );
    }
    let resources = resource_reports(context, resource_records)?;
    let sync = state.as_ref().and_then(|state| state.sync.as_ref());
    let changed = if exists && identity_ok {
        sync.map(|sync| sync_changed(context, tree, &sync.inputs))
            .transpose()?
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let verify = state.as_ref().and_then(|state| state.verify.as_ref());
    Ok(TreeReport {
        target: target.to_string(),
        label: tree.label.to_string(),
        name: tree.name.clone(),
        canonical: tree.canonical,
        tree_id: tree.tree_id.clone(),
        path: tree.path.as_str().to_owned(),
        slot: tree.slot,
        geometry: tree.geometry,
        phase: phase_name(phase).to_owned(),
        branch,
        detached_sha,
        dirty,
        upstream,
        behind_default,
        sync: SyncTreeReport {
            state: sync.map_or_else(
                || "never".to_owned(),
                |sync| {
                    if !sync.ok {
                        "failed"
                    } else if !changed.is_empty() {
                        "stale"
                    } else {
                        "ok"
                    }
                    .to_owned()
                },
            ),
            at: sync.map(|sync| sync.at.clone()),
            changed,
            drift,
        },
        verify: verify.map(|verify| VerifyTreeReport {
            ok: verify.ok,
            at: verify.at.clone(),
        }),
        session,
        session_name: tree.session_name.clone(),
        agent: tree.agent.clone(),
        resources,
        ports,
        disk_kb: disk
            .then(|| wt_sys::fsx::disk_kb(Path::new(tree.path.as_str())))
            .transpose()?,
    })
}

#[derive(Default)]
struct SharedResourceRecords {
    records: BTreeMap<ResourceKey, ResourceRecord>,
}

impl SharedResourceRecords {
    fn read(context: &Context, trees: &[TreeRec]) -> Result<Self, CoreError> {
        if trees.is_empty() {
            return Ok(Self::default());
        }
        let labels = trees
            .iter()
            .map(|tree| tree.label.clone())
            .collect::<BTreeSet<_>>();
        let mut records = BTreeMap::new();
        for label in labels {
            let state = wt_sys::fsx::read_json::<RepoState>(
                &context.home.join(wt_core::model::repo_state_path(&label)),
                "STATE_CORRUPT",
            )?;
            if let Some(state) = state {
                for record in state.resources.into_values() {
                    records.insert(record.key.clone(), record);
                }
            }
        }
        let machine = wt_sys::fsx::read_json::<RepoState>(
            &context.home.join(wt_core::model::machine_state_path()),
            "STATE_CORRUPT",
        )?;
        if let Some(state) = machine {
            for record in state.resources.into_values() {
                records.insert(record.key.clone(), record);
            }
        }
        Ok(Self { records })
    }

    fn for_tree(&self, tree: &TreeRec) -> Vec<ResourceRecord> {
        self.records
            .values()
            .filter(|record| match record.key.tied_to {
                TiedTo::Tree => false,
                TiedTo::Repo => record.key.label.as_ref() == Some(&tree.label),
                TiedTo::Machine => true,
            })
            .cloned()
            .collect()
    }

    fn probe_matching(
        &mut self,
        context: &mut Context,
        tree: &TreeRec,
        matches: impl Fn(&ResourceKey) -> bool,
    ) -> Result<(), CoreError> {
        let keys = self
            .records
            .keys()
            .filter(|key| matches(key))
            .cloned()
            .collect::<Vec<_>>();
        let records = keys
            .iter()
            .filter_map(|key| self.records.remove(key))
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Ok(());
        }
        let target = target_of(tree);
        let mut entered = door::enter(context, Some(&target.to_string()), "probe")?;
        let records = executor::probe_resources(context, &entered, records)?;
        entered.release_gate();
        for record in records {
            self.records.insert(record.key.clone(), record);
        }
        Ok(())
    }
}

fn probe_list_resources(
    context: &mut Context,
    trees: &[TreeRec],
) -> Result<SharedResourceRecords, CoreError> {
    let mut probe_tree_by_label = BTreeMap::<Label, TreeRec>::new();
    for tree in trees {
        let exists = matches!(
            wt_sys::fsx::path_kind(Path::new(tree.path.as_str()))?,
            wt_sys::fsx::PathKind::Directory
        );
        if !exists || !context.identity_ok(tree)? {
            continue;
        }
        let target = target_of(tree);
        let mut entered = door::enter(context, Some(&target.to_string()), "probe")?;
        executor::refresh_all_declarations(context, &entered)?;
        executor::probe_tree_resources(context, &entered)?;
        entered.release_gate();
        probe_tree_by_label
            .entry(tree.label.clone())
            .or_insert_with(|| tree.clone());
    }

    let mut shared = SharedResourceRecords::read(context, trees)?;
    for (label, tree) in &probe_tree_by_label {
        shared.probe_matching(context, tree, |key| {
            key.tied_to == TiedTo::Repo && key.label.as_ref() == Some(label)
        })?;
    }
    if let Some(tree) = probe_tree_by_label.values().next() {
        shared.probe_matching(context, tree, |key| key.tied_to == TiedTo::Machine)?;
    }
    Ok(shared)
}

fn sync_changed(
    context: &Context,
    tree: &wt_core::model::TreeRec,
    inputs: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<String>, CoreError> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let paths = inputs
        .keys()
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    let hashes = context
        .git(Path::new(tree.path.as_str()))?
        .hash_object(&paths)?;
    let mut changed = inputs
        .iter()
        .zip(hashes)
        .filter_map(|((path, prior), current)| (prior != &current).then_some(path.clone()))
        .collect::<Vec<_>>();
    changed.sort();
    Ok(changed)
}

fn to_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn resource_reports(
    context: &Context,
    records: impl IntoIterator<Item = wt_core::resource::ResourceRecord>,
) -> Result<Vec<ResourceReport>, CoreError> {
    let mut reports = records
        .into_iter()
        .map(|record| {
            let holder = executor::exclusive_holder_for_record(context, &record)?;
            Ok(ResourceReport {
                scope: record.key.scope.to_string(),
                task: record.key.task.clone(),
                tied_to: match record.key.tied_to {
                    TiedTo::Tree => "tree",
                    TiedTo::Repo => "repo",
                    TiedTo::Machine => "machine",
                }
                .to_owned(),
                name: record.name().to_owned(),
                state: match record.state {
                    ResourceState::Declared => "declared",
                    ResourceState::Present => "present",
                    ResourceState::Orphaned => "orphaned",
                }
                .to_owned(),
                reason: record.reason.clone(),
                external: record.external,
                undeclared: record.undeclared,
                has_instance: record.instance.is_some(),
                holder,
                last_probe: record.last_probe.as_ref().map(|probe| LastProbeReport {
                    at: probe.at.clone(),
                    result: match &probe.result {
                        ProbeResult::Present => "present",
                        ProbeResult::Absent => "absent",
                        ProbeResult::Failed { .. } => "failed",
                    }
                    .to_owned(),
                }),
                last_error: record.last_error.as_ref().map(|error| LastErrorReport {
                    at: error.at.clone(),
                    event: error.event.clone(),
                    message: error.message.clone(),
                }),
            })
        })
        .collect::<Result<Vec<_>, CoreError>>()?;
    reports.sort_by(|left, right| {
        let order = |tied_to: &str| match tied_to {
            "tree" => 0,
            "repo" => 1,
            _ => 2,
        };
        (order(&left.tied_to), &left.scope, &left.task).cmp(&(
            order(&right.tied_to),
            &right.scope,
            &right.task,
        ))
    });
    Ok(reports)
}

fn session_state(context: &Context, tree: &wt_core::model::TreeRec) -> String {
    if context.settings.session.backend == wt_core::settings::SessionBackend::None {
        return "no".to_owned();
    }
    let timeout = wt_core::model::duration_millis(&context.settings.session.tmux_timeout)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(10));
    match wt_sys::tmux::Tmux::new("tmux", timeout).has_session(&tree.session_name) {
        Ok(true) => "yes",
        Ok(false) => "no",
        Err(error) if error.code.0 == "TOOL_MISSING" => "unknown",
        Err(_) => "unknown",
    }
    .to_owned()
}

pub(crate) const fn phase_name(phase: DerivedPhase) -> &'static str {
    match phase {
        DerivedPhase::Ready => "ready",
        DerivedPhase::Verifying => "verifying",
        DerivedPhase::Initialising => "initialising",
        DerivedPhase::InitInterrupted => "init-interrupted",
        DerivedPhase::Bootstrapping => "bootstrapping",
        DerivedPhase::Interrupted => "interrupted",
        DerivedPhase::Failed => "failed",
        DerivedPhase::Removing => "removing",
        DerivedPhase::RemoveInterrupted => "remove-interrupted",
        DerivedPhase::Creating => "creating",
        DerivedPhase::Claimed => "claimed",
        DerivedPhase::Incomplete => "incomplete",
        DerivedPhase::Replaced => "replaced",
        DerivedPhase::Missing => "missing",
        DerivedPhase::Unmanaged => "unmanaged",
        DerivedPhase::StaleGit => "stale-git",
    }
}

trait ResourceName {
    fn name(&self) -> &str;
}

impl ResourceName for wt_core::resource::ResourceRecord {
    fn name(&self) -> &str {
        self.effective_snapshot().name.as_str()
    }
}
