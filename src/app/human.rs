use serde_json::Value;
use wt_core::doctor::Severity;
use wt_core::report::{
    AdoptData, AnchorData, CloneData, ConfigData, DoctorData, ForgetData, ListData, NewData,
    Notice, PrepareData, PruneData, PruneItemReport, RegisterData, ReleaseData, RemoveData,
    ResourceActionData, SessionsData, StatusData, SyncData, UnregisterData, WhichData,
};

use crate::cli::Command;

#[derive(Clone, Copy)]
pub(crate) enum HumanKind {
    Register,
    Setup,
    Unregister,
    Clone,
    New,
    Prepare,
    Adopt,
    List,
    Status,
    Meta,
    Path,
    Operation,
    Sync,
    Anchor,
    Exec,
    Shell,
    Env,
    Open,
    Edit,
    Close,
    Release,
    Forget,
    Remove,
    Prune,
    Destroy,
    Refresh,
    Doctor,
    Config,
    Which,
    Leases,
    Script,
}

impl From<&Command> for HumanKind {
    fn from(command: &Command) -> Self {
        match command {
            Command::Hook(_) => Self::Operation,
            Command::Setup(_) => Self::Setup,
            Command::Register(_) => Self::Register,
            Command::Unregister(_) => Self::Unregister,
            Command::Clone(_) => Self::Clone,
            Command::New(_) => Self::New,
            Command::Prepare(_) => Self::Prepare,
            Command::Adopt(_) => Self::Adopt,
            Command::List(_) => Self::List,
            Command::Status(_) => Self::Status,
            Command::Meta(_) => Self::Meta,
            Command::Path(_) => Self::Path,
            Command::With(_) => Self::Operation,
            Command::Sync(_) => Self::Sync,
            Command::Anchor(_) => Self::Anchor,
            Command::Exec(_) => Self::Exec,
            Command::Shell(_) => Self::Shell,
            Command::Env(_) => Self::Env,
            Command::Open(_) => Self::Open,
            Command::Edit(_) => Self::Edit,
            Command::Close(_) => Self::Close,
            Command::Release(_) => Self::Release,
            Command::Forget(_) => Self::Forget,
            Command::Remove(_) => Self::Remove,
            Command::Prune(_) => Self::Prune,
            Command::Destroy(_) => Self::Destroy,
            Command::Refresh(_) => Self::Refresh,
            Command::Doctor(_) => Self::Doctor,
            Command::Config(_) => Self::Config,
            Command::Which(_) => Self::Which,
            Command::Leases(_) => Self::Leases,
            Command::ShellInit(_) | Command::Completions(_) => Self::Script,
        }
    }
}

impl HumanKind {
    pub(crate) fn render(self, value: &Value, notices: &[Notice]) -> String {
        match self {
            Self::Register => render_register(decode(value), notices),
            Self::Unregister => render_unregister(decode(value), notices),
            Self::Clone => render_clone(decode(value), notices),
            Self::New => render_new(decode(value), notices),
            Self::Prepare => render_prepare(decode(value), notices),
            Self::Adopt => render_adopt(decode(value), notices),
            Self::List => render_list(decode(value), notices),
            Self::Status => render_status(decode(value), notices),
            Self::Sync => render_sync(decode(value), notices),
            Self::Anchor => render_anchor(decode(value), notices),
            Self::Open | Self::Close => render_sessions(decode(value), notices),
            Self::Release => render_release(decode(value), notices),
            Self::Remove => render_remove(decode(value), notices),
            Self::Prune => render_prune(decode(value), notices),
            Self::Destroy => render_resource("Resource destroyed", value, notices),
            Self::Refresh => render_resource("Resource refreshed", value, notices),
            Self::Doctor => render_doctor(decode(value), notices),
            Self::Config => render_config(decode(value), notices),
            Self::Which => decode::<WhichData>(value)
                .path
                .unwrap_or_else(|| "not found".to_owned()),
            Self::Operation => render_operation(value, notices),
            Self::Path
            | Self::Meta
            | Self::Exec
            | Self::Shell
            | Self::Edit
            | Self::Env
            // `setup` always renders its own text (A76): it is a session of
            // questions, not one operation with a schema.
            | Self::Setup
            | Self::Script => String::new(),
            Self::Forget => render_forget(decode(value), notices),
            Self::Leases => render_leases(value),
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).expect("command output matches its human renderer")
}

fn render_register(data: RegisterData, notices: &[Notice]) -> String {
    let headline = if data.registered {
        format!("Registered {}", data.label)
    } else if data.resumed {
        format!("Resumed registration for {}", data.label)
    } else {
        format!("{} is registered", data.label)
    };
    let declared = [
        ("resources", data.declared.resources.len()),
        ("files", data.declared.files.len()),
        ("bins", data.declared.bin.len()),
        ("ports", data.declared.ports.len()),
        ("copies", data.declared.copy.len()),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .map(|(name, count)| format!("{count} {name}"))
    .collect::<Vec<_>>()
    .join(", ");
    let mut facts = vec![
        ("path", data.path),
        ("phase", data.tree.phase),
        (
            "branch",
            data.tree.branch.unwrap_or_else(|| "detached".to_owned()),
        ),
    ];
    if !declared.is_empty() {
        facts.push(("declared", declared));
    }
    for error in data.config_errors {
        facts.push(("config", format!("{}: {}", error.path, error.message)));
    }
    block(headline, facts, notices)
}

fn render_clone(data: CloneData, notices: &[Notice]) -> String {
    let headline = if data.cloned {
        format!("Cloned and registered {}", data.register.label)
    } else {
        format!("{} is cloned and registered", data.register.label)
    };
    block(
        headline,
        vec![
            ("url", data.url),
            ("hub", data.hub),
            ("path", data.register.path),
            ("phase", data.register.tree.phase),
        ],
        notices,
    )
}

fn render_unregister(data: UnregisterData, notices: &[Notice]) -> String {
    let headline = if data.unregistered {
        format!("Unregistered {}", data.label)
    } else {
        format!("{} was already unregistered", data.label)
    };
    let mut facts = Vec::new();
    if !data.destroyed.is_empty() {
        facts.push(("destroyed", data.destroyed.len().to_string()));
    }
    if !data.artifacts.is_empty() {
        facts.push(("artifacts", data.artifacts.len().to_string()));
    }
    block(headline, facts, notices)
}

fn render_forget(data: ForgetData, notices: &[Notice]) -> String {
    let headline = if data.forgotten {
        format!("Forgot {}", data.target)
    } else {
        format!("Did not forget {}", data.target)
    };
    let facts = if data.artifacts.is_empty() {
        Vec::new()
    } else {
        vec![("artifacts", data.artifacts.len().to_string())]
    };
    block(headline, facts, notices)
}

fn render_new(data: NewData, notices: &[Notice]) -> String {
    let headline = if data.created {
        format!("Created {}", data.tree.target)
    } else if data.resumed {
        format!("Resumed {}", data.tree.target)
    } else {
        format!("{} is ready", data.tree.target)
    };
    let sync = data
        .sync
        .as_ref()
        .map_or_else(|| "skipped".to_owned(), |steps| format_steps(steps));
    let facts = vec![
        ("path", data.tree.path),
        (
            "branch",
            data.tree.branch.unwrap_or_else(|| "detached".to_owned()),
        ),
        ("sync", sync),
    ];
    block(headline, facts, notices)
}

fn render_adopt(data: AdoptData, notices: &[Notice]) -> String {
    let headline = if data.adopted {
        format!("Adopted {}", data.tree.target)
    } else if data.resumed {
        format!("Resumed adoption of {}", data.tree.target)
    } else {
        format!("{} is already adopted", data.tree.target)
    };
    block(
        headline,
        vec![("path", data.tree.path), ("phase", data.tree.phase)],
        notices,
    )
}

fn render_prepare(data: PrepareData, notices: &[Notice]) -> String {
    let headline = if data.adopted {
        format!("Prepared {}", data.tree.target)
    } else if data.resumed {
        format!("Resumed preparation of {}", data.tree.target)
    } else {
        format!("{} is prepared", data.tree.target)
    };
    let sync = data
        .sync
        .as_ref()
        .map_or_else(|| "skipped".to_owned(), |steps| format_steps(steps));
    block(
        headline,
        vec![
            ("path", data.tree.path),
            (
                "branch",
                data.tree.branch.unwrap_or_else(|| "detached".to_owned()),
            ),
            ("sync", sync),
        ],
        notices,
    )
}

fn render_status(data: StatusData, notices: &[Notice]) -> String {
    let mut facts = vec![
        ("path", data.tree.path),
        (
            "branch",
            data.tree
                .branch
                .or(data.tree.detached_sha)
                .unwrap_or_else(|| "unknown".to_owned()),
        ),
        ("sync", format_sync(&data.tree.sync)),
        ("session", data.tree.session),
    ];
    if let Some(dirty) = data.tree.dirty {
        facts.push((
            "changes",
            format!("{} modified, {} untracked", dirty.modified, dirty.untracked),
        ));
    }
    if let Some(after_new) = data.tree.after_new {
        facts.push((
            "after_new",
            format!("{} (log {})", after_new.state, after_new.log),
        ));
    }
    if !data.tree.meta.is_empty() {
        facts.push((
            "meta",
            data.tree
                .meta
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if !data.tree.ports.is_empty() {
        facts.push((
            "ports",
            data.tree
                .ports
                .iter()
                .map(|port| format!("{}={}", port.name, port.port))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if !data.tree.resources.is_empty() {
        let held = data
            .tree
            .resources
            .iter()
            .filter_map(|resource| {
                resource.holder.as_ref().map(|holder| {
                    format!("{}={} (holder {holder})", resource.resource, resource.state)
                })
            })
            .collect::<Vec<_>>();
        facts.push((
            "resources",
            if held.is_empty() {
                data.tree.resources.len().to_string()
            } else {
                held.join(", ")
            },
        ));
    }
    for error in data.config_errors {
        facts.push(("config", format!("{}: {}", error.path, error.message)));
    }
    block(
        format!("{} is {}", data.tree.target, data.tree.phase),
        facts,
        notices,
    )
}

fn render_list(data: ListData, _notices: &[Notice]) -> String {
    render_list_with_meta(data, None)
}

pub(crate) fn render_list_meta(data: ListData, key: &str) -> String {
    render_list_with_meta(data, Some(key))
}

fn render_list_with_meta(data: ListData, meta_key: Option<&str>) -> String {
    let show_holders = data.trees.iter().any(|tree| {
        tree.resources
            .iter()
            .any(|resource| resource.holder.is_some())
    });
    if show_holders {
        let rows = data
            .trees
            .into_iter()
            .map(|tree| {
                let resources = tree
                    .resources
                    .iter()
                    .filter_map(|resource| {
                        resource
                            .holder
                            .as_ref()
                            .map(|holder| format!("{}:{holder}", resource.resource))
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let mut row = vec![
                    tree.target,
                    tree.phase,
                    tree.branch.unwrap_or_else(|| "detached".to_owned()),
                    tree.sync.state,
                    resources,
                ];
                if let Some(key) = meta_key {
                    row.push(tree.meta.get(key).cloned().unwrap_or_default());
                }
                row.push(tree.path);
                row
            })
            .collect::<Vec<_>>();
        if let Some(key) = meta_key {
            table(
                "Registered trees",
                ["target", "phase", "branch", "sync", "holders", key, "path"],
                rows,
            )
        } else {
            table(
                "Registered trees",
                ["target", "phase", "branch", "sync", "holders", "path"],
                rows,
            )
        }
    } else {
        let rows = data
            .trees
            .into_iter()
            .map(|tree| {
                let mut row = vec![
                    tree.target,
                    tree.phase,
                    tree.branch.unwrap_or_else(|| "detached".to_owned()),
                    tree.sync.state,
                ];
                if let Some(key) = meta_key {
                    row.push(tree.meta.get(key).cloned().unwrap_or_default());
                }
                row.push(tree.path);
                row
            })
            .collect::<Vec<_>>();
        if let Some(key) = meta_key {
            table(
                "Registered trees",
                ["target", "phase", "branch", "sync", key, "path"],
                rows,
            )
        } else {
            table(
                "Registered trees",
                ["target", "phase", "branch", "sync", "path"],
                rows,
            )
        }
    }
}

fn render_sync(data: SyncData, notices: &[Notice]) -> String {
    let headline = if data.ran {
        format!("Synced {}", data.target)
    } else {
        format!("{} is already in sync", data.target)
    };
    let mut facts = Vec::new();
    if !data.steps.is_empty() {
        facts.push(("steps", format_steps(&data.steps)));
    }
    if !data.inputs.is_empty() {
        facts.push((
            "inputs",
            data.inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    block(headline, facts, notices)
}

fn render_anchor(data: AnchorData, notices: &[Notice]) -> String {
    let stayed = notices.iter().find(|notice| {
        matches!(
            notice.code.as_str(),
            "ANCHOR_DIRTY" | "ANCHOR_OFF_DEFAULT" | "ANCHOR_DIVERGED" | "ANCHOR_NOT_MOVED"
        )
    });
    let headline = if !data.refreshed {
        format!("{} is being refreshed by another wt", data.label)
    } else if data.moved.is_some() {
        format!("Refreshed {} at the default branch's new tip", data.label)
    } else if stayed.is_some() {
        format!("{} stays where it is", data.label)
    } else {
        format!("{} is at the default branch's tip", data.label)
    };
    let mut facts = Vec::new();
    if let Some(reason) = stayed {
        facts.push(("reason", reason.message.clone()));
    }
    if let Some(fetched) = data.fetched {
        facts.push(("fetch", if fetched { "ok" } else { "failed" }.to_owned()));
    }
    if let Some(moved) = &data.moved {
        facts.push((
            "moved",
            format!(
                "{} -> {}",
                &moved.from[..moved.from.len().min(12)],
                &moved.to[..moved.to.len().min(12)]
            ),
        ));
    }
    facts.push((
        "warm",
        match data.warm.as_str() {
            "fresh" => "already current".to_owned(),
            "none" => "no warm hook".to_owned(),
            other => other.to_owned(),
        },
    ));
    if let Some(swept) = &data.swept {
        if swept.units > 0 || swept.incremental > 0 {
            facts.push((
                "swept",
                format!(
                    "{} units, {} incremental directories, {}",
                    swept.units,
                    swept.incremental,
                    size(swept.kb)
                ),
            ));
        }
    }
    block(headline, facts, notices)
}

fn render_sessions(data: SessionsData, notices: &[Notice]) -> String {
    if data.sessions.is_empty() {
        return block("No sessions changed", Vec::new(), notices);
    }
    let mut rows = Vec::new();
    for session in data.sessions {
        match session {
            wt_core::report::SessionReport::Open(open) => rows.push((
                open.target,
                if open.created {
                    "created".to_owned()
                } else if open.existing {
                    "already open".to_owned()
                } else {
                    "opened".to_owned()
                },
            )),
            wt_core::report::SessionReport::Closed(closed) => rows.push((
                closed.target,
                if closed.closed {
                    "closed".to_owned()
                } else {
                    "already closed".to_owned()
                },
            )),
            wt_core::report::SessionReport::Failed(failed) => {
                rows.push((failed.target, format!("failed ({})", failed.code)))
            }
        }
    }
    let headline = if rows.len() == 1 {
        format!("Session {} for {}", rows[0].1, rows[0].0)
    } else {
        format!("Updated {} sessions", rows.len())
    };
    let facts = if rows.len() > 1 {
        rows.into_iter()
            .map(|(target, state)| ("session", format!("{target}: {state}")))
            .collect()
    } else {
        Vec::new()
    };
    block(headline, facts, notices)
}

fn render_remove(data: RemoveData, notices: &[Notice]) -> String {
    let headline = if data.removed {
        format!("Removed {}", data.target)
    } else {
        notices
            .iter()
            .find(|notice| matches!(notice.code.as_str(), "ALREADY_REMOVED" | "REMOVE_DECLINED"))
            .map(|notice| notice.message.clone())
            .unwrap_or_else(|| format!("Removal of {} did not proceed", data.target))
    };
    let mut facts = Vec::new();
    if !data.destroyed.is_empty() {
        facts.push(("destroyed", data.destroyed.len().to_string()));
    }
    if !data.orphans_kept.is_empty() {
        facts.push(("orphans", data.orphans_kept.join(", ")));
    }
    if data.branch_deleted {
        facts.push(("branch", "deleted".to_owned()));
    }
    if let Some(branch) = data.branch_kept {
        facts.push(("kept", format!("branch {branch}")));
    }
    if data.session_closed {
        facts.push(("session", "closed".to_owned()));
    }
    block(headline, facts, notices)
}

fn render_release(data: ReleaseData, notices: &[Notice]) -> String {
    let headline = if data.released {
        format!("Released {}", data.target)
    } else {
        format!("Did not release {}", data.target)
    };
    let mut facts = Vec::new();
    if !data.destroyed.is_empty() {
        facts.push(("destroyed", data.destroyed.len().to_string()));
    }
    if !data.artifacts.is_empty() {
        facts.push(("artifacts", data.artifacts.len().to_string()));
    }
    if data.session_closed {
        facts.push(("session", "closed".to_owned()));
    }
    block(headline, facts, notices)
}

fn render_prune(data: PruneData, _notices: &[Notice]) -> String {
    let count = data.items.len();
    let headline = if data.applied {
        format!(
            "Applied prune plan to {count} {}",
            plural(count, "item", "items")
        )
    } else {
        format!("Prune plan has {count} {}", plural(count, "item", "items"))
    };
    prune_table(headline, &data.items)
}

/// The plan `prune` asks about before applying it, in the same shape its
/// report takes afterwards.
pub(crate) fn prune_plan(items: &[PruneItemReport]) -> String {
    let count = items.len();
    prune_table(
        format!("Prune plan has {count} {}", plural(count, "item", "items")),
        items,
    )
}

fn prune_table(headline: String, items: &[PruneItemReport]) -> String {
    let rows = items
        .iter()
        .map(|item| {
            vec![
                item.target.clone(),
                item.action.clone(),
                item.reasons.join(", "),
            ]
        })
        .collect();
    table(headline, ["target", "action", "reason"], rows)
}

fn render_resource(headline: &str, value: &Value, notices: &[Notice]) -> String {
    let Ok(data) = serde_json::from_value::<ResourceActionData>(value.clone()) else {
        return block("Resource unchanged", Vec::new(), notices);
    };
    block(
        format!("{headline}: {}", data.resource),
        vec![
            ("target", data.target),
            ("scope", data.scope),
            ("state", format!("{} -> {}", data.before, data.after)),
        ],
        notices,
    )
}

fn render_doctor(data: DoctorData, notices: &[Notice]) -> String {
    let headline = if data.findings.is_empty() {
        "Doctor found no problems".to_owned()
    } else {
        format!(
            "Doctor found {} {}, {} {}, {} {}",
            data.counts.error,
            plural(data.counts.error, "error", "errors"),
            data.counts.warn,
            plural(data.counts.warn, "warning", "warnings"),
            data.counts.info,
            plural(data.counts.info, "note", "notes")
        )
    };
    let mut facts = Vec::new();
    for finding in data.findings {
        let severity = match finding.severity {
            Severity::Error => "error",
            Severity::Warn => "warning",
            Severity::Info => "note",
        };
        facts.push((
            severity,
            format!("{} {} - {}", finding.code, finding.subject, finding.message),
        ));
        facts.push(("next", finding.remedy));
    }
    block(headline, facts, notices)
}

fn render_config(data: ConfigData, _notices: &[Notice]) -> String {
    let mut rows = Vec::new();
    for entry in data.entries {
        if let Some(object) = entry.value.as_object() {
            for (key, value) in object {
                rows.push(vec![
                    key.clone(),
                    entry.scope.clone(),
                    entry.layer.clone(),
                    summarize_config(key, value),
                ]);
            }
        } else {
            rows.push(vec![
                entry.key,
                entry.scope,
                entry.layer,
                summarize_value(&entry.value),
            ]);
        }
    }
    table(
        format!("Effective config for {}", data.target),
        ["key", "scope", "layer", "value"],
        rows,
    )
}

fn render_operation(value: &Value, notices: &[Notice]) -> String {
    let _ = notices;
    value
        .get("target")
        .and_then(Value::as_str)
        .map_or_else(String::new, |target| format!("Updated {target}"))
}

fn render_leases(value: &Value) -> String {
    let rows = value["leases"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|lease| {
            let holder = lease["holder"]
                .as_object()
                .map(|holder| {
                    format!(
                        "{}:{}",
                        holder.get("pid").and_then(Value::as_u64).unwrap_or(0),
                        holder
                            .get("process_start")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    )
                })
                .unwrap_or_default();
            vec![
                lease["pool"].as_str().unwrap_or("").to_owned(),
                lease["slot"].to_string(),
                lease["name"].as_str().unwrap_or("").to_owned(),
                lease["kind"].as_str().unwrap_or("free").to_owned(),
                holder,
            ]
        })
        .collect::<Vec<_>>();
    table(
        "Resource leases",
        ["pool", "slot", "name", "kind", "holder"],
        rows,
    )
}

fn format_sync(sync: &wt_core::report::SyncTreeReport) -> String {
    let mut parts = vec![sync.state.clone()];
    if !sync.changed.is_empty() {
        parts.push(format!("changed: {}", sync.changed.join(", ")));
    }
    if !sync.drift.is_empty() {
        parts.push(format!("drift: {}", sync.drift.join(", ")));
    }
    parts.join("; ")
}

fn format_steps(steps: &[wt_core::report::StepReport]) -> String {
    if steps.is_empty() {
        "no steps".to_owned()
    } else {
        let passed = steps.iter().filter(|step| step.status == "ok").count();
        let skipped = steps
            .iter()
            .filter(|step| matches!(step.status.as_str(), "skipped" | "present"))
            .count();
        if skipped == 0 {
            format!("{passed}/{} passed", steps.len())
        } else {
            format!("{passed} passed, {skipped} skipped")
        }
    }
}

fn summarize_config(key: &str, value: &Value) -> String {
    if key == "env" {
        return value
            .as_object()
            .map(|values| {
                if values.is_empty() {
                    "-".to_owned()
                } else {
                    values.keys().cloned().collect::<Vec<_>>().join(", ")
                }
            })
            .unwrap_or_default();
    }
    summarize_value(value)
}

fn summarize_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => {
            if values.is_empty() {
                "-".to_owned()
            } else {
                values
                    .iter()
                    .map(|value| match value {
                        Value::String(value) => value.clone(),
                        _ => compact(value),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
        Value::Object(values) => {
            let keys = values.keys().cloned().collect::<Vec<_>>();
            if keys.is_empty() {
                "-".to_owned()
            } else {
                keys.join(", ")
            }
        }
    }
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// A plan rendered for a consent prompt, in the summary shape of §14.1.
pub(crate) fn consent_block(
    headline: impl Into<String>,
    facts: Vec<(&'static str, String)>,
) -> String {
    block(headline, facts, &[])
}

fn block(
    headline: impl Into<String>,
    facts: Vec<(&'static str, String)>,
    _notices: &[Notice],
) -> String {
    let mut output = headline.into();
    if facts.is_empty() {
        return output;
    }
    let width = facts.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    for (key, value) in facts {
        output.push_str(&format!("\n  {key:<width$}  {value}"));
    }
    output
}

/// A disk size for people: whole KB or MB while small, GB to one decimal
/// once it passes a gigabyte, so large sweeps read at a glance.
pub(crate) fn size(kb: u64) -> String {
    if kb < 1024 {
        format!("{kb} KB")
    } else if kb < 1024 * 1024 {
        format!("{} MB", kb / 1024)
    } else {
        format!("{:.1} GB", kb as f64 / (1024.0 * 1024.0))
    }
}

fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

fn table<const N: usize>(
    headline: impl Into<String>,
    headers: [&str; N],
    rows: Vec<Vec<String>>,
) -> String {
    let mut output = headline.into();
    if rows.is_empty() {
        return output;
    }
    let mut widths = headers.map(str::len);
    for row in &rows {
        for (index, value) in row.iter().enumerate().take(N) {
            widths[index] = widths[index].max(value.len());
        }
    }
    output.push('\n');
    output.push_str("  ");
    output.push_str(&table_row(&headers.map(str::to_owned), &widths));
    for row in rows {
        output.push('\n');
        output.push_str("  ");
        output.push_str(&table_row(&row, &widths));
    }
    output
}

fn table_row<const N: usize>(row: &[String], widths: &[usize; N]) -> String {
    row.iter()
        .enumerate()
        .take(N)
        .map(|(index, value)| {
            if index + 1 == N {
                value.clone()
            } else {
                format!("{value:<width$}", width = widths[index])
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
mod tests {
    use super::size;

    #[test]
    fn sizes_step_up_to_gigabytes() {
        assert_eq!(size(512), "512 KB");
        assert_eq!(size(3_672_064), "3.5 GB");
        assert_eq!(size(12938 * 1024), "12.6 GB");
        assert_eq!(size(900 * 1024), "900 MB");
    }
}
