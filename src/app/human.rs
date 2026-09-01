use serde_json::Value;
use wt_core::doctor::Severity;
use wt_core::report::{
    AdoptData, CloneData, ConfigData, DoctorData, ForgetData, ListData, LocksData, NewData, Notice,
    PruneData, RegisterData, RemoveData, ResourceActionData, SessionsData, StatusData, SyncData,
    TasksData, UnregisterData, WhichData,
};

use crate::cli::Command;

#[derive(Clone, Copy)]
pub(crate) enum HumanKind {
    Register,
    Unregister,
    Clone,
    New,
    Adopt,
    List,
    Status,
    Meta,
    Path,
    Run,
    Sync,
    Exec,
    Shell,
    Env,
    Open,
    Edit,
    Close,
    Forget,
    Remove,
    Prune,
    Destroy,
    Refresh,
    Doctor,
    Tasks,
    Config,
    Which,
    Locks,
    Script,
}

impl From<&Command> for HumanKind {
    fn from(command: &Command) -> Self {
        match command {
            Command::Register(_) => Self::Register,
            Command::Unregister(_) => Self::Unregister,
            Command::Clone(_) => Self::Clone,
            Command::New(_) => Self::New,
            Command::Adopt(_) => Self::Adopt,
            Command::List(_) => Self::List,
            Command::Status(_) => Self::Status,
            Command::Meta(_) => Self::Meta,
            Command::Path(_) => Self::Path,
            Command::Run(_)
            | Command::Test(_)
            | Command::Lint(_)
            | Command::Fmt(_)
            | Command::Build(_) => Self::Run,
            Command::Sync(_) => Self::Sync,
            Command::Exec(_) => Self::Exec,
            Command::Shell(_) => Self::Shell,
            Command::Env(_) => Self::Env,
            Command::Open(_) => Self::Open,
            Command::Edit(_) => Self::Edit,
            Command::Close(_) => Self::Close,
            Command::Forget(_) => Self::Forget,
            Command::Remove(_) => Self::Remove,
            Command::Prune(_) => Self::Prune,
            Command::Destroy(_) => Self::Destroy,
            Command::Refresh(_) => Self::Refresh,
            Command::Doctor(_) => Self::Doctor,
            Command::Tasks(_) => Self::Tasks,
            Command::Config(_) => Self::Config,
            Command::Which(_) => Self::Which,
            Command::Locks(_) => Self::Locks,
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
            Self::Adopt => render_adopt(decode(value), notices),
            Self::List => render_list(decode(value), notices),
            Self::Status => render_status(decode(value), notices),
            Self::Sync => render_sync(decode(value), notices),
            Self::Open | Self::Close => render_sessions(decode(value), notices),
            Self::Remove => render_remove(decode(value), notices),
            Self::Prune => render_prune(decode(value), notices),
            Self::Destroy => render_resource("Resource destroyed", value, notices),
            Self::Refresh => render_resource("Resource refreshed", value, notices),
            Self::Doctor => render_doctor(decode(value), notices),
            Self::Tasks => render_tasks(decode(value), notices),
            Self::Config => render_config(decode(value), notices),
            Self::Which => decode::<WhichData>(value)
                .path
                .unwrap_or_else(|| "not found".to_owned()),
            Self::Run => render_run(value, notices),
            Self::Path
            | Self::Meta
            | Self::Exec
            | Self::Shell
            | Self::Edit
            | Self::Env
            | Self::Script => String::new(),
            Self::Forget => render_forget(decode(value), notices),
            Self::Locks => render_locks(decode(value), notices),
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
        ("tasks", data.declared.tasks.len()),
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
    let verify = data.verify.as_ref().map(|verify| {
        if verify.ok {
            format!("passed ({})", format_steps(&verify.steps))
        } else {
            "failed".to_owned()
        }
    });
    let mut facts = vec![
        ("path", data.tree.path),
        (
            "branch",
            data.tree.branch.unwrap_or_else(|| "detached".to_owned()),
        ),
        ("sync", sync),
    ];
    if let Some(verify) = verify {
        facts.push(("verify", verify));
    }
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
    if let Some(build) = data.tree.build {
        facts.push(("build", format!("{} (log {})", build.state, build.log)));
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
        let held =
            data.tree
                .resources
                .iter()
                .filter_map(|resource| {
                    resource.holder.as_ref().map(|holder| {
                        format!("{}={} (holder {holder})", resource.task, resource.state)
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
    if !data.tasks.is_empty() {
        facts.push(("tasks", data.tasks.len().to_string()));
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
                            .map(|holder| format!("{}:{holder}", resource.task))
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
    if let Some(cache) = data.cache_deleted {
        facts.push(("cache", format!("deleted {cache}")));
    }
    block(headline, facts, notices)
}

fn render_prune(data: PruneData, notices: &[Notice]) -> String {
    let headline = if data.applied {
        format!("Applied prune plan to {} items", data.items.len())
    } else {
        format!("Prune plan has {} items", data.items.len())
    };
    let facts = data
        .items
        .into_iter()
        .map(|item| {
            let reasons = if item.reasons.is_empty() {
                String::new()
            } else {
                format!(" ({})", item.reasons.join(", "))
            };
            ("item", format!("{}: {}{reasons}", item.target, item.action))
        })
        .collect();
    block(headline, facts, notices)
}

fn render_resource(headline: &str, value: &Value, notices: &[Notice]) -> String {
    let Ok(data) = serde_json::from_value::<ResourceActionData>(value.clone()) else {
        return block("Resource unchanged", Vec::new(), notices);
    };
    block(
        format!("{headline}: {}", data.task),
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

fn render_tasks(data: TasksData, _notices: &[Notice]) -> String {
    let rows = data
        .tasks
        .into_iter()
        .map(|task| {
            vec![
                task.id,
                task.scope,
                task.origin,
                task.cwd,
                task.needs.join(","),
            ]
        })
        .collect::<Vec<_>>();
    table(
        format!("Effective tasks for {}", data.target),
        ["task", "scope", "layer", "cwd", "needs"],
        rows,
    )
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

fn render_locks(data: LocksData, _notices: &[Notice]) -> String {
    let rows = data
        .locks
        .into_iter()
        .map(|lock| {
            let state = lock.slots.map_or_else(
                || if lock.held { "held" } else { "free" }.to_owned(),
                |slots| format!("held {}/{slots}", lock.held_slots.unwrap_or(0)),
            );
            let holders = if lock.slots.is_some() {
                lock.holders
                    .into_iter()
                    .map(|slot| {
                        slot.holder.map_or_else(
                            || format!("{}:unknown", slot.slot),
                            |holder| format!("{}:{} {}", slot.slot, holder.pid, holder.verb),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            } else if lock.held {
                lock.holder
                    .map(|holder| format!("{} {}", holder.pid, holder.verb))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            vec![lock.level.to_string(), lock.name, state, holders, lock.path]
        })
        .collect::<Vec<_>>();
    table(
        "Coordination locks",
        ["level", "name", "state", "holder", "path"],
        rows,
    )
}

fn render_run(value: &Value, notices: &[Notice]) -> String {
    if value.get("steps").is_some() && value.get("target").is_none() {
        let task = value["task"].as_str().unwrap_or("task");
        let args = value["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(Value::to_string)
            .collect::<Vec<_>>();
        let headline = if args.is_empty() {
            format!("Plan for {task}")
        } else {
            format!("Plan for {task} -- {}", args.join(" "))
        };
        let rows = value["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|step| {
                vec![
                    step["id"].as_str().unwrap_or("").to_owned(),
                    step["scope"].as_str().unwrap_or("").to_owned(),
                    step["origin"].as_str().unwrap_or("").to_owned(),
                    step["cwd"].as_str().unwrap_or("").to_owned(),
                ]
            })
            .collect::<Vec<_>>();
        return table(headline, ["task", "scope", "layer", "cwd"], rows);
    }
    let _ = notices;
    String::new()
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
