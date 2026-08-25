use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{doctor::Finding, env::Activation, error::CoreError, model::Geometry};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WtMeta {
    pub schema: u8,
    pub version: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Warn,
    Info,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub level: NoticeLevel,
    pub code: String,
    pub subject: Option<String>,
    pub message: String,
    #[serde(skip)]
    pub guidance: Option<NoticeGuidance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NoticeGuidance {
    MissingBin {
        target: String,
        path: String,
        has_build_task: bool,
    },
}

impl Notice {
    pub fn next_step(&self) -> Option<String> {
        match self.guidance.as_ref()? {
            NoticeGuidance::MissingBin {
                target,
                path,
                has_build_task,
            } if *has_build_task => Some(format!("run `wt build {target}` to create {path}")),
            NoticeGuidance::MissingBin { path, .. } => {
                Some(format!("create {path} before running tree binaries"))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorReport {
    pub class: crate::ExitClass,
    pub exit: u8,
    pub code: String,
    pub message: String,
    pub remedy: String,
    pub details: Value,
}

impl From<CoreError> for ErrorReport {
    fn from(error: CoreError) -> Self {
        Self {
            class: error.class,
            exit: error.exit(),
            code: error.code.0,
            message: error.message,
            remedy: error.remedy,
            details: error.details,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope<D> {
    pub wt: WtMeta,
    pub ok: bool,
    pub command: String,
    pub data: Option<D>,
    pub notices: Vec<Notice>,
    pub error: Option<ErrorReport>,
}

impl<D> Envelope<D> {
    pub fn success(command: impl Into<String>, version: impl Into<String>, data: D) -> Self {
        Self {
            wt: WtMeta {
                schema: 1,
                version: version.into(),
            },
            ok: true,
            command: command.into(),
            data: Some(data),
            notices: Vec::new(),
            error: None,
        }
    }

    pub fn failure(
        command: impl Into<String>,
        version: impl Into<String>,
        error: CoreError,
    ) -> Self {
        Self {
            wt: WtMeta {
                schema: 1,
                version: version.into(),
            },
            ok: false,
            command: command.into(),
            data: None,
            notices: Vec::new(),
            error: Some(error.into()),
        }
    }

    pub fn partial_failure(
        command: impl Into<String>,
        version: impl Into<String>,
        data: D,
        error: CoreError,
    ) -> Self {
        Self {
            wt: WtMeta {
                schema: 1,
                version: version.into(),
            },
            ok: false,
            command: command.into(),
            data: Some(data),
            notices: Vec::new(),
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChildReport {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LastProbeReport {
    pub at: String,
    pub result: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirtyReport {
    pub modified: u64,
    pub untracked: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpstreamReport {
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncTreeReport {
    pub state: String,
    pub at: Option<String>,
    pub changed: Vec<String>,
    pub drift: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifyTreeReport {
    pub ok: bool,
    pub at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceReport {
    pub scope: String,
    pub task: String,
    pub tied_to: String,
    pub name: String,
    pub state: String,
    pub reason: Option<String>,
    pub external: bool,
    pub undeclared: bool,
    pub has_instance: bool,
    pub last_probe: Option<LastProbeReport>,
    pub last_error: Option<LastErrorReport>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LastErrorReport {
    pub at: String,
    pub event: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortReport {
    pub name: String,
    pub port: u16,
    pub bound: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TreeReport {
    pub target: String,
    pub label: String,
    pub name: String,
    pub canonical: bool,
    pub tree_id: String,
    pub path: String,
    pub slot: u32,
    pub geometry: Geometry,
    pub phase: String,
    pub branch: Option<String>,
    pub detached_sha: Option<String>,
    pub dirty: Option<DirtyReport>,
    pub upstream: Option<UpstreamReport>,
    pub behind_default: Option<u32>,
    pub sync: SyncTreeReport,
    pub verify: Option<VerifyTreeReport>,
    pub session: String,
    pub session_name: String,
    pub agent: Option<String>,
    pub resources: Vec<ResourceReport>,
    pub ports: Vec<PortReport>,
    pub disk_kb: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StepReport {
    pub id: String,
    pub scope: String,
    pub status: String,
    pub child: Option<ChildReport>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DryRunStepReport {
    pub id: String,
    pub scope: String,
    pub origin: String,
    pub cwd: String,
    pub run: Option<crate::config::Command>,
    pub exists: Option<crate::config::Command>,
    pub lock: Option<String>,
    pub sys_locks: Vec<String>,
    pub resource: bool,
    pub tied_to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DryRunData {
    pub task: String,
    pub steps: Vec<DryRunStepReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigErrorReport {
    pub path: String,
    pub line: u64,
    pub col: u64,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskInfoReport {
    pub id: String,
    pub scope: String,
    pub origin: String,
    pub cwd: String,
    pub needs: Vec<String>,
    pub resource: bool,
    pub tied_to: Option<String>,
    pub lock: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusData {
    #[serde(flatten)]
    pub tree: TreeReport,
    pub tasks: Vec<TaskInfoReport>,
    pub config_errors: Vec<ConfigErrorReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeclaredResourceReport {
    pub scope: String,
    pub task: String,
    pub tied_to: String,
    pub snapshot_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeclaredReport {
    pub tasks: Vec<String>,
    pub resources: Vec<DeclaredResourceReport>,
    pub env: Vec<String>,
    pub files: Vec<String>,
    pub bin: Vec<String>,
    pub ports: Vec<String>,
    pub copy: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegisterData {
    pub label: String,
    pub path: String,
    pub gitdir_id: String,
    pub registered: bool,
    pub resumed: bool,
    pub tree: TreeReport,
    pub declared: DeclaredReport,
    pub config_errors: Vec<ConfigErrorReport>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloneData {
    pub url: String,
    pub cloned: bool,
    #[serde(flatten)]
    pub register: RegisterData,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnregisterData {
    pub label: String,
    pub unregistered: bool,
    pub destroyed: Vec<DestroyedReport>,
    pub artifacts: Vec<ArtifactReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DestroyedReport {
    pub scope: String,
    pub task: String,
    pub state: String,
    pub child: Option<ChildReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactReport {
    pub path: String,
    pub action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewVerifyReport {
    pub ok: bool,
    pub steps: Vec<StepReport>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewData {
    pub tree: TreeReport,
    pub created: bool,
    pub resumed: bool,
    pub sync: Option<Vec<StepReport>>,
    pub verify: Option<NewVerifyReport>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdoptData {
    pub tree: TreeReport,
    pub adopted: bool,
    pub resumed: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoveData {
    pub target: String,
    pub removed: bool,
    pub destroyed: Vec<DestroyedReport>,
    pub orphans_kept: Vec<String>,
    pub branch_deleted: bool,
    pub session_closed: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncData {
    pub target: String,
    pub ran: bool,
    pub steps: Vec<StepReport>,
    pub inputs: Vec<SyncInputReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncInputReport {
    pub path: String,
    pub hash: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunData {
    pub target: String,
    pub task: String,
    pub child: Option<ChildReport>,
    pub log: Option<String>,
    pub steps: Vec<StepReport>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceActionData {
    pub target: String,
    pub scope: String,
    pub task: String,
    pub before: String,
    pub after: String,
    pub child: Option<ChildReport>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionsData {
    pub sessions: Vec<SessionReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionReport {
    Open(OpenSessionReport),
    Closed(ClosedSessionReport),
    Failed(FailedSessionReport),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenSessionReport {
    pub target: String,
    pub name: String,
    pub created: bool,
    pub existing: bool,
    pub agent: Option<String>,
    pub foreground: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FailedSessionReport {
    pub target: String,
    pub name: String,
    pub failed: bool,
    pub code: String,
    pub message: String,
    pub remedy: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClosedSessionReport {
    pub target: String,
    pub session: String,
    pub closed: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvData {
    pub target: String,
    pub set: Vec<String>,
    pub overrode: Vec<String>,
    pub restored: Vec<String>,
    pub missing_bins: Vec<String>,
    pub rendered: Vec<String>,
    pub bins: Vec<BinReport>,
    pub ports: Vec<PortReport>,
    pub env: BTreeMap<String, String>,
    pub activation: Activation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BinReport {
    pub dir: String,
    pub exists: bool,
    pub executables: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HolderReport {
    pub pid: u32,
    pub target: String,
    pub verb: String,
    pub since: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListLockReport {
    pub name: String,
    pub label: String,
    pub holder: HolderReport,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListData {
    pub trees: Vec<TreeReport>,
    pub locks: Vec<ListLockReport>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathData {
    pub target: String,
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WhichData {
    pub target: String,
    pub cmd: String,
    pub path: Option<String>,
    pub in_bin: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TasksData {
    pub target: String,
    pub tasks: Vec<TaskInfoReport>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigData {
    pub target: String,
    pub entries: Vec<ConfigEntryReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigEntryReport {
    pub key: String,
    pub scope: String,
    pub layer: String,
    pub value: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocksData {
    pub locks: Vec<LockReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LockReport {
    pub level: u8,
    pub name: String,
    pub path: String,
    pub held: bool,
    pub holder: Option<HolderReport>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PruneData {
    pub applied: bool,
    pub items: Vec<PruneItemReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PruneItemReport {
    pub target: String,
    pub reasons: Vec<String>,
    pub action: String,
    pub result: Option<Value>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DoctorData {
    pub findings: Vec<Finding>,
    pub counts: crate::doctor::FindingCounts,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptData {
    pub shell: String,
    pub script: String,
}

pub fn sort_trees(trees: &mut [TreeReport]) {
    trees.sort_by(|left, right| {
        (&left.label, !left.canonical, &left.name).cmp(&(
            &right.label,
            !right.canonical,
            &right.name,
        ))
    });
    for tree in trees {
        tree.resources.sort_by(|left, right| {
            (tied_order(&left.tied_to), &left.scope, &left.task).cmp(&(
                tied_order(&right.tied_to),
                &right.scope,
                &right.task,
            ))
        });
    }
}

fn tied_order(value: &str) -> u8 {
    if value == "tree" {
        0
    } else {
        1
    }
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(value)?;
    canonicalise_value(&mut value);
    serde_json::to_string(&value)
}

pub fn sort_unlisted_array(values: &mut [Value]) {
    for value in values.iter_mut() {
        canonicalise_value(value);
    }
    values.sort_by_cached_key(|value| serde_json::to_string(value).unwrap_or_default());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArrayOrder {
    Trees,
    Resources,
    LexicalStrings,
    Notices,
    Findings,
    Targets,
    Locks,
    ConfigEntries,
    ConfigErrors,
    Canonical,
    Semantic,
}

pub fn sort_array(values: &mut [Value], order: ArrayOrder) {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let number =
        |value: &Value, key: &str| value.get(key).and_then(Value::as_u64).unwrap_or_default();
    match order {
        ArrayOrder::Trees => values.sort_by_key(|value| {
            (
                text(value, "label"),
                !value
                    .get("canonical")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                text(value, "name"),
            )
        }),
        ArrayOrder::Resources => values.sort_by_key(|value| {
            (
                u8::from(text(value, "tied_to") != "tree"),
                text(value, "scope"),
                text(value, "task"),
            )
        }),
        ArrayOrder::LexicalStrings => {
            values.sort_by_key(|value| value.as_str().unwrap_or_default().to_owned())
        }
        ArrayOrder::Notices => values.sort_by_key(|value| {
            (
                u8::from(text(value, "level") != "warn"),
                text(value, "code"),
                text(value, "subject"),
                text(value, "message"),
            )
        }),
        ArrayOrder::Findings => values.sort_by_key(|value| {
            let severity = match text(value, "severity").as_str() {
                "error" => 0,
                "warn" => 1,
                _ => 2,
            };
            (severity, text(value, "code"), text(value, "subject"))
        }),
        ArrayOrder::Targets => values.sort_by_key(|value| text(value, "target")),
        ArrayOrder::Locks => {
            values.sort_by_key(|value| (number(value, "level"), text(value, "name")))
        }
        ArrayOrder::ConfigEntries => values.sort_by_key(|value| {
            (
                text(value, "key"),
                text(value, "scope"),
                layer_order(&text(value, "layer")),
            )
        }),
        ArrayOrder::ConfigErrors => values.sort_by_key(|value| {
            (
                text(value, "path"),
                number(value, "line"),
                number(value, "col"),
                text(value, "message"),
            )
        }),
        ArrayOrder::Canonical => sort_unlisted_array(values),
        ArrayOrder::Semantic => {}
    }
}

fn layer_order(layer: &str) -> u8 {
    match layer {
        "tree" => 0,
        "user" => 1,
        "repo" => 2,
        "adapter" => 3,
        _ => 4,
    }
}

fn canonicalise_value(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                canonicalise_value(value);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                canonicalise_value(value);
            }
            let old = std::mem::take(map);
            let mut entries: Vec<_> = old.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            map.extend(entries);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::{FindingCounts, Severity};

    fn tree(label: &str, name: &str, canonical: bool) -> TreeReport {
        TreeReport {
            target: if canonical {
                label.to_owned()
            } else {
                format!("{label}/{name}")
            },
            label: label.to_owned(),
            name: name.to_owned(),
            canonical,
            tree_id: "TREE".to_owned(),
            path: "/tree".to_owned(),
            slot: 0,
            geometry: Geometry {
                base: 20000,
                stride: 16,
                port_base: 20000,
            },
            phase: "ready".to_owned(),
            branch: Some("main".to_owned()),
            detached_sha: None,
            dirty: Some(DirtyReport {
                modified: 0,
                untracked: 0,
            }),
            upstream: Some(UpstreamReport {
                ahead: 0,
                behind: 0,
            }),
            behind_default: Some(0),
            sync: SyncTreeReport {
                state: "never".to_owned(),
                at: None,
                changed: Vec::new(),
                drift: Vec::new(),
            },
            verify: Some(VerifyTreeReport {
                ok: true,
                at: "TIME".to_owned(),
            }),
            session: "no".to_owned(),
            session_name: "session".to_owned(),
            agent: None,
            resources: vec![ResourceReport {
                scope: ".".to_owned(),
                task: "db".to_owned(),
                tied_to: "tree".to_owned(),
                name: "repo_db".to_owned(),
                state: "declared".to_owned(),
                reason: None,
                external: false,
                undeclared: false,
                has_instance: false,
                last_probe: Some(LastProbeReport {
                    at: "TIME".to_owned(),
                    result: "absent".to_owned(),
                }),
                last_error: Some(LastErrorReport {
                    at: "TIME".to_owned(),
                    event: "probe".to_owned(),
                    message: "none".to_owned(),
                }),
            }],
            ports: vec![PortReport {
                name: "http".to_owned(),
                port: 20_000,
                bound: Some(false),
            }],
            disk_kb: None,
        }
    }

    fn activation() -> Activation {
        Activation {
            v: 1,
            target: "repo/work".to_owned(),
            home: "/home".to_owned(),
            applied: BTreeMap::new(),
            prior: BTreeMap::new(),
        }
    }

    fn step(id: &str) -> StepReport {
        StepReport {
            id: id.to_owned(),
            scope: ".".to_owned(),
            status: "ok".to_owned(),
            child: Some(ChildReport {
                code: Some(0),
                signal: None,
            }),
            duration_ms: 12,
        }
    }

    fn task_info() -> TaskInfoReport {
        TaskInfoReport {
            id: "test".to_owned(),
            scope: ".".to_owned(),
            origin: "repo".to_owned(),
            cwd: ".".to_owned(),
            needs: vec!["build".to_owned()],
            resource: false,
            tied_to: None,
            lock: Some("tests".to_owned()),
            description: Some("run tests".to_owned()),
        }
    }

    fn config_error() -> ConfigErrorReport {
        ConfigErrorReport {
            path: ".wt.toml".to_owned(),
            line: 2,
            col: 3,
            message: "example diagnostic".to_owned(),
        }
    }

    fn destroyed() -> DestroyedReport {
        DestroyedReport {
            scope: ".".to_owned(),
            task: "db".to_owned(),
            state: "dropped".to_owned(),
            child: Some(ChildReport {
                code: Some(0),
                signal: None,
            }),
        }
    }

    fn holder() -> HolderReport {
        HolderReport {
            pid: 42,
            target: "repo/work".to_owned(),
            verb: "run".to_owned(),
            since: "TIME".to_owned(),
        }
    }

    fn snap<T: Serialize>(name: &str, data: T) {
        let mut envelope = Envelope::success("command", "VERSION", data);
        envelope.notices.push(Notice {
            level: NoticeLevel::Info,
            code: "EXAMPLE".to_owned(),
            subject: Some("repo/work".to_owned()),
            message: "example notice".to_owned(),
            guidance: None,
        });
        insta::assert_json_snapshot!(name, envelope);
    }

    #[test]
    fn snapshots_every_success_data_schema() {
        let t = tree("repo", "work", false);
        let register = RegisterData {
            label: "repo".into(),
            path: "/repo".into(),
            gitdir_id: "HASH".into(),
            registered: true,
            resumed: false,
            tree: t.clone(),
            declared: DeclaredReport {
                tasks: vec!["test".to_owned()],
                resources: vec![DeclaredResourceReport {
                    scope: ".".to_owned(),
                    task: "db".to_owned(),
                    tied_to: "tree".to_owned(),
                    snapshot_keys: vec!["PATH".to_owned(), "WT_ROOT".to_owned()],
                }],
                env: vec!["PORT".to_owned()],
                files: vec!["generated.toml".to_owned()],
                bin: vec!["target/debug".to_owned()],
                ports: vec!["http".to_owned()],
                copy: vec![".mcp.json".to_owned()],
            },
            config_errors: vec![config_error()],
        };
        snap("register", register.clone());
        snap(
            "clone",
            CloneData {
                url: "https://example.test/repo".into(),
                cloned: true,
                register,
            },
        );
        snap(
            "unregister",
            UnregisterData {
                label: "repo".into(),
                unregistered: true,
                destroyed: vec![destroyed()],
                artifacts: vec![ArtifactReport {
                    path: "generated.toml".to_owned(),
                    action: "deleted".to_owned(),
                }],
            },
        );
        snap(
            "new",
            NewData {
                tree: t.clone(),
                created: true,
                resumed: false,
                sync: Some(vec![step("sync")]),
                verify: Some(NewVerifyReport {
                    ok: true,
                    steps: vec![step("test")],
                }),
            },
        );
        snap(
            "adopt",
            AdoptData {
                tree: t.clone(),
                adopted: true,
                resumed: false,
            },
        );
        snap(
            "remove",
            RemoveData {
                target: "repo/work".into(),
                removed: true,
                destroyed: vec![destroyed()],
                orphans_kept: vec!["legacy".to_owned()],
                branch_deleted: false,
                session_closed: false,
            },
        );
        snap(
            "sync",
            SyncData {
                target: "repo/work".into(),
                ran: true,
                steps: vec![step("sync")],
                inputs: vec![SyncInputReport {
                    path: "Cargo.lock".to_owned(),
                    hash: "HASH".to_owned(),
                }],
            },
        );
        snap(
            "run",
            RunData {
                target: "repo/work".into(),
                task: "test".into(),
                child: Some(ChildReport {
                    code: Some(0),
                    signal: None,
                }),
                log: Some(".wt/logs/test.log".to_owned()),
                steps: vec![step("test")],
            },
        );
        snap(
            "run_dry",
            DryRunData {
                task: "test".into(),
                steps: vec![DryRunStepReport {
                    id: "test".into(),
                    scope: ".".into(),
                    origin: "repo".into(),
                    cwd: ".".into(),
                    run: Some(crate::config::Command::Shell("cargo test".into())),
                    exists: None,
                    lock: None,
                    sys_locks: Vec::new(),
                    resource: false,
                    tied_to: None,
                }],
            },
        );
        let resource_action = ResourceActionData {
            target: "repo/work".into(),
            scope: ".".into(),
            task: "db".into(),
            before: "declared".into(),
            after: "present".into(),
            child: None,
        };
        snap("destroy", resource_action.clone());
        snap("refresh", resource_action);
        snap(
            "sessions",
            SessionsData {
                sessions: vec![SessionReport::Open(OpenSessionReport {
                    target: "repo/work".into(),
                    name: "session".into(),
                    created: true,
                    existing: false,
                    agent: None,
                    foreground: false,
                })],
            },
        );
        snap(
            "close_sessions",
            SessionsData {
                sessions: vec![SessionReport::Closed(ClosedSessionReport {
                    target: "repo/work".into(),
                    session: "session".into(),
                    closed: true,
                })],
            },
        );
        snap(
            "env",
            EnvData {
                target: "repo/work".into(),
                set: vec!["WT_ROOT".to_owned()],
                overrode: vec!["PATH".to_owned()],
                restored: vec!["WT_TARGET".to_owned()],
                missing_bins: vec!["missing/bin".to_owned()],
                rendered: vec!["generated.toml".to_owned()],
                bins: vec![BinReport {
                    dir: "target/debug".to_owned(),
                    exists: true,
                    executables: vec!["wt".to_owned()],
                }],
                ports: vec![PortReport {
                    name: "http".to_owned(),
                    port: 20016,
                    bound: None,
                }],
                env: BTreeMap::from([("WT_ROOT".to_owned(), "/tree".to_owned())]),
                activation: activation(),
            },
        );
        snap(
            "list",
            ListData {
                trees: vec![t.clone()],
                locks: vec![ListLockReport {
                    name: "repo/work".to_owned(),
                    label: "repo".to_owned(),
                    holder: holder(),
                }],
            },
        );
        snap(
            "status",
            StatusData {
                tree: t,
                tasks: vec![task_info()],
                config_errors: vec![config_error()],
            },
        );
        snap(
            "path",
            PathData {
                target: "repo/work".into(),
                path: "/tree".into(),
            },
        );
        snap(
            "which",
            WhichData {
                target: "repo/work".into(),
                cmd: "cargo".into(),
                path: None,
                in_bin: false,
            },
        );
        snap(
            "tasks",
            TasksData {
                target: "repo/work".into(),
                tasks: vec![task_info()],
            },
        );
        snap(
            "config",
            ConfigData {
                target: "repo/work".into(),
                entries: vec![ConfigEntryReport {
                    key: "task.test".to_owned(),
                    scope: ".".to_owned(),
                    layer: "repo".to_owned(),
                    value: serde_json::json!({"needs":["build"]}),
                }],
            },
        );
        snap(
            "locks",
            LocksData {
                locks: vec![LockReport {
                    level: 1,
                    name: "repo/work".to_owned(),
                    path: "/home/locks/repo/work.lock".to_owned(),
                    held: true,
                    holder: Some(holder()),
                }],
            },
        );
        snap(
            "prune",
            PruneData {
                applied: false,
                items: vec![PruneItemReport {
                    target: "repo/old".to_owned(),
                    reasons: vec!["missing".to_owned()],
                    action: "remove".to_owned(),
                    result: Some(serde_json::json!({"removed":true})),
                }],
            },
        );
        snap(
            "doctor",
            DoctorData {
                findings: vec![Finding::new(
                    Severity::Info,
                    "NO_COORDINATION",
                    "repo",
                    "healthy",
                    "none",
                )],
                counts: FindingCounts {
                    error: 0,
                    warn: 0,
                    info: 1,
                },
            },
        );
        let script = ScriptData {
            shell: "zsh".into(),
            script: "true".into(),
        };
        snap("shell_init", script.clone());
        snap("completions", script);
        insta::assert_json_snapshot!(
            "error",
            Envelope::<Value>::failure(
                "new",
                "VERSION",
                CoreError::new(
                    crate::ExitClass::Conflict,
                    "NAME_TAKEN",
                    "target exists",
                    "choose another name",
                ),
            )
        );
    }

    #[test]
    fn stable_ordering_rules_are_applied() {
        let mut trees = vec![
            tree("b", "z", false),
            tree("a", "work", false),
            tree("a", "canonical", true),
        ];
        sort_trees(&mut trees);
        assert_eq!(
            trees
                .iter()
                .map(|tree| (&tree.label, tree.canonical, &tree.name))
                .collect::<Vec<_>>(),
            [
                (&"a".to_owned(), true, &"canonical".to_owned()),
                (&"a".to_owned(), false, &"work".to_owned()),
                (&"b".to_owned(), false, &"z".to_owned())
            ]
        );
        let value = BTreeMap::from([("z", 1), ("a", 2)]);
        assert_eq!(canonical_json(&value).unwrap(), r#"{"a":2,"z":1}"#);
        let mut unlisted = vec![serde_json::json!({"z": 1}), serde_json::json!({"a": 2})];
        sort_unlisted_array(&mut unlisted);
        assert_eq!(
            unlisted,
            [serde_json::json!({"a": 2}), serde_json::json!({"z": 1})]
        );
        let mut notices = vec![
            serde_json::json!({"level":"info","code":"A","subject":null,"message":"m"}),
            serde_json::json!({"level":"warn","code":"Z","subject":null,"message":"m"}),
        ];
        sort_array(&mut notices, ArrayOrder::Notices);
        assert_eq!(notices[0]["level"], "warn");
        let mut resources = vec![
            serde_json::json!({"tied_to":"repo","scope":".","task":"a"}),
            serde_json::json!({"tied_to":"tree","scope":"z","task":"z"}),
        ];
        sort_array(&mut resources, ArrayOrder::Resources);
        assert_eq!(resources[0]["tied_to"], "tree");
        let mut semantic = vec![serde_json::json!("z"), serde_json::json!("a")];
        sort_array(&mut semantic, ArrayOrder::Semantic);
        assert_eq!(semantic, [serde_json::json!("z"), serde_json::json!("a")]);
    }

    #[test]
    fn every_declared_array_order_and_nested_map_canonicalisation_is_covered() {
        let cases = [
            (
                ArrayOrder::LexicalStrings,
                vec![serde_json::json!("z"), serde_json::json!("a")],
                serde_json::json!("a"),
            ),
            (
                ArrayOrder::Findings,
                vec![
                    serde_json::json!({"severity":"info","code":"A","subject":"x"}),
                    serde_json::json!({"severity":"error","code":"Z","subject":"x"}),
                ],
                serde_json::json!({"severity":"error","code":"Z","subject":"x"}),
            ),
            (
                ArrayOrder::Targets,
                vec![
                    serde_json::json!({"target":"z"}),
                    serde_json::json!({"target":"a"}),
                ],
                serde_json::json!({"target":"a"}),
            ),
            (
                ArrayOrder::Locks,
                vec![
                    serde_json::json!({"level":2,"name":"a"}),
                    serde_json::json!({"level":1,"name":"z"}),
                ],
                serde_json::json!({"level":1,"name":"z"}),
            ),
            (
                ArrayOrder::ConfigEntries,
                vec![
                    serde_json::json!({"key":"a","scope":".","layer":"repo"}),
                    serde_json::json!({"key":"a","scope":".","layer":"tree"}),
                ],
                serde_json::json!({"key":"a","scope":".","layer":"tree"}),
            ),
            (
                ArrayOrder::ConfigErrors,
                vec![
                    serde_json::json!({"path":"z","line":1,"col":1,"message":"a"}),
                    serde_json::json!({"path":"a","line":2,"col":1,"message":"z"}),
                ],
                serde_json::json!({"path":"a","line":2,"col":1,"message":"z"}),
            ),
            (
                ArrayOrder::Canonical,
                vec![serde_json::json!({"z":1}), serde_json::json!({"a":2})],
                serde_json::json!({"a":2}),
            ),
        ];
        for (order, mut values, first) in cases {
            sort_array(&mut values, order);
            assert_eq!(values[0], first, "{order:?}");
        }

        let value = serde_json::json!({"z":{"y":1,"a":2},"a":[{"z":3,"a":4}]});
        let once = canonical_json(&value).unwrap();
        let reparsed: Value = serde_json::from_str(&once).unwrap();
        assert_eq!(canonical_json(&reparsed).unwrap(), once);
        assert_eq!(once, r#"{"a":[{"a":4,"z":3}],"z":{"a":2,"y":1}}"#);
    }
}
