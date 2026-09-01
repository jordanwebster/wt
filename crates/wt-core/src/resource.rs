use serde::{Deserialize, Serialize};

use crate::{
    config::{Command, Exclusive, TiedTo},
    model::{name_snake, EnvMap, Label, RelDir},
    CoreError, ExitClass,
};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ResourceKey {
    pub label: Option<Label>,
    pub tied_to: TiedTo,
    /// Tree-tied resources use the address name; wider scopes use null.
    pub name: Option<String>,
    pub scope: RelDir,
    pub task: String,
}

pub fn default_name_template(key: &ResourceKey) -> String {
    let scoped_task = if key.scope.as_str() == "." {
        key.task.clone()
    } else {
        format!("{}/{}", key.scope, key.task)
    };
    let prefix = match key.tied_to {
        TiedTo::Tree => "{{name_short()}}",
        TiedTo::Repo => "{{label()}}",
        TiedTo::Machine => "machine",
    };
    format!("{prefix}_{}", name_snake(&scoped_task))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExpandedCommand {
    Shell { shell: String },
    Argv { argv: Vec<String> },
}

impl From<Command> for ExpandedCommand {
    fn from(value: Command) -> Self {
        match value {
            Command::Shell(shell) => Self::Shell { shell },
            Command::Argv(argv) => Self::Argv { argv },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRoots {
    pub tree: String,
    pub home: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    pub schema: u8,
    pub key: ResourceKey,
    #[serde(default)]
    pub exclusive: Option<Exclusive>,
    pub name: String,
    pub cwd_rel: RelDir,
    pub exists: Option<ExpandedCommand>,
    pub destroy: ExpandedCommand,
    pub run: Option<ExpandedCommand>,
    pub env: EnvMap,
    pub bin_dirs: Vec<String>,
    pub bin_exes: Vec<String>,
    pub roots: SnapshotRoots,
    #[serde(default)]
    pub recorded_sequence: u64,
    pub recorded_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceState {
    Declared,
    Present,
    Orphaned,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChildStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeFailure {
    Exit { code: i32 },
    Timeout,
    Spawn { message: String },
}

impl ProbeFailure {
    fn message(&self) -> String {
        match self {
            Self::Exit { code } => format!("probe exited {code}"),
            Self::Timeout => "probe timed out".to_owned(),
            Self::Spawn { message } => message.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeResult {
    Present,
    Absent,
    Failed { failure: ProbeFailure },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub at: String,
    pub result: ProbeResult,
}

impl Probe {
    pub fn present(at: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            result: ProbeResult::Present,
        }
    }

    pub fn absent(at: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            result: ProbeResult::Absent,
        }
    }

    pub fn failed_exit(at: impl Into<String>, code: i32) -> Result<Self, CoreError> {
        if code < 2 {
            return Err(CoreError::new(
                ExitClass::Internal,
                "PROBE_CONTRACT",
                "a failed probe exit must be at least 2",
                "map exit 0 to present and exit 1 to absent",
            ));
        }
        Ok(Self {
            at: at.into(),
            result: ProbeResult::Failed {
                failure: ProbeFailure::Exit { code },
            },
        })
    }

    pub fn failed_timeout(at: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            result: ProbeResult::Failed {
                failure: ProbeFailure::Timeout,
            },
        }
    }

    pub fn failed_spawn(at: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            result: ProbeResult::Failed {
                failure: ProbeFailure::Spawn {
                    message: message.into(),
                },
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LastError {
    pub at: String,
    pub event: String,
    pub message: String,
    pub child: Option<ChildStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceRecord {
    pub key: ResourceKey,
    pub declaration: ResourceSnapshot,
    pub instance: Option<ResourceSnapshot>,
    pub state: ResourceState,
    pub reason: Option<String>,
    pub external: bool,
    pub undeclared: bool,
    pub last_probe: Option<Probe>,
    pub last_error: Option<LastError>,
    pub since: String,
}

impl ResourceRecord {
    pub fn declared(snapshot: ResourceSnapshot) -> Self {
        Self {
            key: snapshot.key.clone(),
            since: snapshot.recorded_at.clone(),
            declaration: snapshot,
            instance: None,
            state: ResourceState::Declared,
            reason: None,
            external: false,
            undeclared: false,
            last_probe: None,
            last_error: None,
        }
    }

    pub fn effective_snapshot(&self) -> &ResourceSnapshot {
        self.instance.as_ref().unwrap_or(&self.declaration)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectResult {
    Ok {
        at: String,
        child: Option<ChildStatus>,
    },
    Failed {
        at: String,
        message: String,
        reason: String,
        child: Option<ChildStatus>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Declare(Box<ResourceSnapshot>),
    Probe(Probe),
    Run {
        probe: Probe,
        run: Option<EffectResult>,
        confirm: Option<Probe>,
    },
    Destroy {
        teardown: bool,
        probe: Probe,
        destroy: Option<EffectResult>,
        confirm: Option<Probe>,
    },
    Refresh {
        probe: Probe,
        destroy: Option<EffectResult>,
        confirm_destroy: Option<Probe>,
        run: Option<EffectResult>,
        confirm_run: Option<Probe>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    None,
    Run,
    Destroy,
    Probe,
    PollReady,
    Dropped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepResult {
    pub record: Option<ResourceRecord>,
    pub action: Action,
    pub notices: Vec<String>,
    pub finding: Option<String>,
    pub error: Option<CoreError>,
}

impl StepResult {
    fn keep(record: ResourceRecord) -> Self {
        Self {
            record: Some(record),
            action: Action::None,
            notices: Vec::new(),
            finding: None,
            error: None,
        }
    }

    fn dropped() -> Self {
        Self {
            record: None,
            action: Action::Dropped,
            notices: Vec::new(),
            finding: None,
            error: None,
        }
    }

    fn error(record: ResourceRecord, code: &str, message: impl Into<String>) -> Self {
        Self {
            record: Some(record),
            action: Action::None,
            notices: Vec::new(),
            finding: None,
            error: Some(CoreError::new(
                ExitClass::ChildFailed,
                code,
                message,
                "inspect the resource state and retry the lifecycle command",
            )),
        }
    }
}

/// Advances one cell of the three-state table in SPEC §10.4. Records never
/// persist an in-progress state; the resource lock serialises each call and a
/// fresh probe re-establishes truth after a crash.
pub fn step(record: Option<ResourceRecord>, event: Event) -> StepResult {
    let Some(mut record) = record else {
        return match event {
            Event::Declare(snapshot) => StepResult::keep(ResourceRecord::declared(*snapshot)),
            _ => StepResult {
                record: None,
                action: Action::None,
                notices: Vec::new(),
                finding: None,
                error: Some(CoreError::new(
                    ExitClass::Internal,
                    "RESOURCE_RECORD_MISSING",
                    "resource event has no declaration record",
                    "refresh the resource declarations",
                )),
            },
        };
    };

    match event {
        Event::Declare(snapshot) => {
            record.declaration = *snapshot;
            record.undeclared = false;
            StepResult::keep(record)
        }
        Event::Probe(probe) => observe(record, probe),
        Event::Run {
            probe,
            run: run_result,
            confirm,
        } => run(record, probe, run_result, confirm),
        Event::Destroy {
            teardown,
            probe,
            destroy: destroy_result,
            confirm,
        } => destroy(record, teardown, probe, destroy_result, confirm),
        Event::Refresh {
            probe,
            destroy,
            confirm_destroy,
            run: run_result,
            confirm_run,
        } => refresh(
            record,
            probe,
            destroy,
            confirm_destroy,
            run_result,
            confirm_run,
        ),
    }
}

fn record_probe(record: &mut ResourceRecord, probe: &Probe, event: &str) {
    record.last_probe = Some(probe.clone());
    if let ProbeResult::Failed { failure } = &probe.result {
        record.last_error = Some(LastError {
            at: probe.at.clone(),
            event: event.to_owned(),
            message: failure.message(),
            child: None,
        });
    }
}

fn clear_absent(record: &mut ResourceRecord) {
    record.state = ResourceState::Declared;
    record.instance = None;
    record.reason = None;
    record.external = false;
}

fn observe(mut record: ResourceRecord, probe: Probe) -> StepResult {
    record_probe(&mut record, &probe, "probe");
    match probe.result {
        ProbeResult::Present => {
            if record.instance.is_none() {
                record.instance = Some(record.declaration.clone());
                record.external = true;
            }
            if record.state != ResourceState::Orphaned {
                record.state = ResourceState::Present;
                record.reason = None;
            }
            StepResult::keep(record)
        }
        ProbeResult::Absent => {
            let gone = record.state != ResourceState::Declared;
            clear_absent(&mut record);
            let mut result = StepResult::keep(record);
            if gone {
                result.notices.push("RESOURCE_GONE".to_owned());
            }
            result
        }
        ProbeResult::Failed { .. } => {
            let mut result = StepResult::keep(record);
            result.finding = Some("RESOURCE_PROBE_FAILED".to_owned());
            result
        }
    }
}

fn run(
    mut record: ResourceRecord,
    probe: Probe,
    run_result: Option<EffectResult>,
    confirm: Option<Probe>,
) -> StepResult {
    if record.state == ResourceState::Orphaned {
        return StepResult::error(
            record,
            "RESOURCE_ORPHANED",
            "orphaned resource cannot be run",
        );
    }

    let was_present = record.state == ResourceState::Present;
    record_probe(&mut record, &probe, "run_probe");
    match probe.result {
        ProbeResult::Present => {
            if record.instance.is_none() {
                record.instance = Some(record.declaration.clone());
                record.external = true;
            }
            record.state = ResourceState::Present;
            record.reason = None;
            StepResult::keep(record)
        }
        ProbeResult::Failed { .. } => {
            StepResult::error(record, "RESOURCE_PROBE_FAILED", "resource probe failed")
        }
        ProbeResult::Absent => {
            clear_absent(&mut record);
            let mut notices = Vec::new();
            if was_present {
                notices.push("RESOURCE_GONE".to_owned());
            }
            if record.declaration.run.is_none() {
                notices.push("RESOURCE_DECLARED_EXTERNAL".to_owned());
                let mut result = StepResult::keep(record);
                result.notices = notices;
                return result;
            }

            // This record must be committed before the executor performs Run.
            record.instance = Some(record.declaration.clone());
            let Some(run_result) = run_result else {
                let mut result = StepResult::keep(record);
                result.action = Action::Run;
                result.notices = notices;
                return result;
            };
            match run_result {
                EffectResult::Failed {
                    at, message, child, ..
                } => {
                    record.last_error = Some(LastError {
                        at,
                        event: "run".to_owned(),
                        message,
                        child,
                    });
                    let mut result =
                        StepResult::error(record, "TASK_FAILED", "resource creation failed");
                    result.notices = notices;
                    result
                }
                EffectResult::Ok { .. } => {
                    let Some(confirm) = confirm else {
                        let mut result = StepResult::keep(record);
                        result.action = Action::PollReady;
                        result.notices = notices;
                        return result;
                    };
                    record_probe(&mut record, &confirm, "run_confirm");
                    match confirm.result {
                        ProbeResult::Present => {
                            record.state = ResourceState::Present;
                            record.external = false;
                            record.reason = None;
                            let mut result = StepResult::keep(record);
                            result.notices = notices;
                            result
                        }
                        ProbeResult::Absent => {
                            clear_absent(&mut record);
                            record.last_error = Some(LastError {
                                at: confirm.at,
                                event: "run".to_owned(),
                                message: "absent_after_run".to_owned(),
                                child: None,
                            });
                            let mut result = StepResult::error(
                                record,
                                "NOT_READY",
                                "resource remained absent after creation",
                            );
                            result.notices = notices;
                            result
                        }
                        ProbeResult::Failed { failure } => {
                            record.last_error = Some(LastError {
                                at: confirm.at,
                                event: "run".to_owned(),
                                message: failure.message(),
                                child: None,
                            });
                            let mut result = StepResult::error(
                                record,
                                "RESOURCE_PROBE_FAILED",
                                "confirming probe failed",
                            );
                            result.notices = notices;
                            result
                        }
                    }
                }
            }
        }
    }
}

fn destroy(
    mut record: ResourceRecord,
    teardown: bool,
    probe: Probe,
    destroy_result: Option<EffectResult>,
    confirm: Option<Probe>,
) -> StepResult {
    record_probe(&mut record, &probe, "destroy_probe");
    match probe.result {
        ProbeResult::Failed { .. } => {
            if teardown {
                record.state = ResourceState::Orphaned;
                record.reason = Some("probe_failed".to_owned());
                StepResult::keep(record)
            } else {
                StepResult::error(record, "RESOURCE_PROBE_FAILED", "resource probe failed")
            }
        }
        ProbeResult::Absent => {
            clear_absent(&mut record);
            if teardown {
                StepResult::dropped()
            } else {
                StepResult::keep(record)
            }
        }
        ProbeResult::Present => {
            if record.instance.is_none() {
                record.instance = Some(record.declaration.clone());
                record.external = true;
            }
            let Some(destroy_result) = destroy_result else {
                let mut result = StepResult::keep(record);
                result.action = Action::Destroy;
                return result;
            };
            match destroy_result {
                EffectResult::Failed {
                    at,
                    message,
                    reason,
                    child,
                } => {
                    record.state = ResourceState::Orphaned;
                    record.reason = Some(reason);
                    record.last_error = Some(LastError {
                        at,
                        event: "destroy".to_owned(),
                        message,
                        child,
                    });
                    StepResult::error(record, "DESTROY_FAILED", "resource destroy failed")
                }
                EffectResult::Ok { .. } => {
                    let Some(confirm) = confirm else {
                        let mut result = StepResult::keep(record);
                        result.action = Action::Probe;
                        return result;
                    };
                    record_probe(&mut record, &confirm, "destroy_confirm");
                    match confirm.result {
                        ProbeResult::Absent => {
                            clear_absent(&mut record);
                            if teardown || record.undeclared {
                                StepResult::dropped()
                            } else {
                                StepResult::keep(record)
                            }
                        }
                        ProbeResult::Present => {
                            record.state = ResourceState::Orphaned;
                            record.reason = Some("still_present".to_owned());
                            record.last_error = Some(LastError {
                                at: confirm.at,
                                event: "destroy".to_owned(),
                                message: "still_present".to_owned(),
                                child: None,
                            });
                            StepResult::error(
                                record,
                                "DESTROY_FAILED",
                                "resource is still present after destroy",
                            )
                        }
                        ProbeResult::Failed { failure } => {
                            record.state = ResourceState::Orphaned;
                            record.reason = Some("probe_failed".to_owned());
                            record.last_error = Some(LastError {
                                at: confirm.at,
                                event: "destroy".to_owned(),
                                message: failure.message(),
                                child: None,
                            });
                            StepResult::error(
                                record,
                                "RESOURCE_PROBE_FAILED",
                                "destroy confirmation probe failed",
                            )
                        }
                    }
                }
            }
        }
    }
}

fn refresh(
    record: ResourceRecord,
    probe: Probe,
    destroy_result: Option<EffectResult>,
    confirm_destroy: Option<Probe>,
    run_result: Option<EffectResult>,
    confirm_run: Option<Probe>,
) -> StepResult {
    if record.state == ResourceState::Orphaned {
        return StepResult::error(
            record,
            "RESOURCE_ORPHANED",
            "orphaned resource cannot be refreshed",
        );
    }
    let after_destroy = destroy(record, false, probe, destroy_result, confirm_destroy);
    if after_destroy.error.is_some() || after_destroy.action != Action::None {
        return after_destroy;
    }
    let Some(record) = after_destroy.record else {
        return after_destroy;
    };
    if record.declaration.run.is_none() {
        return StepResult::keep(record);
    }
    let at = record
        .last_probe
        .as_ref()
        .map(|probe| probe.at.clone())
        .unwrap_or_else(|| record.since.clone());
    run(record, Probe::absent(at), run_result, confirm_run)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(with_run: bool) -> ResourceSnapshot {
        ResourceSnapshot {
            schema: 1,
            key: ResourceKey {
                label: Some(Label::new("repo").unwrap()),
                tied_to: TiedTo::Tree,
                name: Some("work".to_owned()),
                scope: RelDir::new(".").unwrap(),
                task: "db".to_owned(),
            },
            exclusive: None,
            name: "repo_db".to_owned(),
            cwd_rel: RelDir::new(".").unwrap(),
            exists: Some(ExpandedCommand::Shell {
                shell: "probe".to_owned(),
            }),
            destroy: ExpandedCommand::Shell {
                shell: "drop".to_owned(),
            },
            run: with_run.then(|| ExpandedCommand::Shell {
                shell: "create".to_owned(),
            }),
            env: EnvMap::new(),
            bin_dirs: Vec::new(),
            bin_exes: Vec::new(),
            roots: SnapshotRoots {
                tree: "/tree".to_owned(),
                home: "/home".to_owned(),
            },
            recorded_sequence: 1,
            recorded_at: "T0".to_owned(),
        }
    }

    fn record(state: ResourceState, with_run: bool) -> ResourceRecord {
        let mut record = ResourceRecord::declared(snapshot(with_run));
        record.state = state;
        if state != ResourceState::Declared {
            record.instance = Some(record.declaration.clone());
        }
        if state == ResourceState::Orphaned {
            record.reason = Some("old_failure".to_owned());
        }
        record
    }

    fn ok(at: &str) -> EffectResult {
        EffectResult::Ok {
            at: at.to_owned(),
            child: None,
        }
    }

    fn failed(at: &str, reason: &str) -> EffectResult {
        EffectResult::Failed {
            at: at.to_owned(),
            message: reason.to_owned(),
            reason: reason.to_owned(),
            child: Some(ChildStatus {
                code: Some(1),
                signal: None,
            }),
        }
    }

    #[test]
    fn table_driven_test_covers_every_three_state_event_cell() {
        struct Case {
            name: &'static str,
            state: ResourceState,
            with_run: bool,
            initial_instance: Option<bool>,
            undeclared: bool,
            event: Event,
            expected: Option<ResourceState>,
            error: Option<&'static str>,
            action: Action,
            instance: bool,
            external: bool,
            reason: Option<&'static str>,
            notices: &'static [&'static str],
            finding: Option<&'static str>,
            last_error: Option<&'static str>,
        }
        let cases = vec![
            Case {
                name: "declared/run/present-freezes-external",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::present("1"),
                    run: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::None,
                instance: true,
                external: true,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/run/absent-freezes-before-effect",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::absent("1"),
                    run: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::Run,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/run/run-fail",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::absent("1"),
                    run: Some(failed("2", "run_failed")),
                    confirm: None,
                },
                expected: Some(ResourceState::Declared),
                error: Some("TASK_FAILED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: Some("run_failed"),
            },
            Case {
                name: "declared/run/run-ok-polls",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::absent("1"),
                    run: Some(ok("2")),
                    confirm: None,
                },
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::PollReady,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/run/run-ok-present",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::absent("1"),
                    run: Some(ok("2")),
                    confirm: Some(Probe::present("3")),
                },
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::None,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/run/absent-after-run",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::absent("1"),
                    run: Some(ok("2")),
                    confirm: Some(Probe::absent("3")),
                },
                expected: Some(ResourceState::Declared),
                error: Some("NOT_READY"),
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: Some("absent_after_run"),
            },
            Case {
                name: "declared/run/confirm-failed",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::absent("1"),
                    run: Some(ok("2")),
                    confirm: Some(Probe::failed_exit("3", 2).unwrap()),
                },
                expected: Some(ResourceState::Declared),
                error: Some("RESOURCE_PROBE_FAILED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: Some("probe exited 2"),
            },
            Case {
                name: "declared/run/no-run-external",
                state: ResourceState::Declared,
                with_run: false,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::absent("1"),
                    run: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &["RESOURCE_DECLARED_EXTERNAL"],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/run/probe-failed",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::failed_exit("1", 2).unwrap(),
                    run: Some(ok("2")),
                    confirm: None,
                },
                expected: Some(ResourceState::Declared),
                error: Some("RESOURCE_PROBE_FAILED"),
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: Some("probe exited 2"),
            },
            Case {
                name: "declared/probe/present",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Probe(Probe::present("1")),
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::None,
                instance: true,
                external: true,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/probe/absent-clears-stale-instance-without-notice",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: Some(true),
                undeclared: false,
                event: Event::Probe(Probe::absent("1")),
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/probe/failed-finding",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Probe(Probe::failed_exit("1", 2).unwrap()),
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: Some("RESOURCE_PROBE_FAILED"),
                last_error: Some("probe exited 2"),
            },
            Case {
                name: "declared/destroy/absent-stays",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: true,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::absent("1"),
                    destroy: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/destroy/teardown-drop",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: true,
                    probe: Probe::absent("1"),
                    destroy: None,
                    confirm: None,
                },
                expected: None,
                error: None,
                action: Action::Dropped,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "declared/destroy/failed-probe-teardown-orphans",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: true,
                    probe: Probe::failed_exit("1", 2).unwrap(),
                    destroy: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Orphaned),
                error: None,
                action: Action::None,
                instance: false,
                external: false,
                reason: Some("probe_failed"),
                notices: &[],
                finding: None,
                last_error: Some("probe exited 2"),
            },
            Case {
                name: "declared/destroy/failed-probe-nonteardown-errors",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::failed_exit("1", 2).unwrap(),
                    destroy: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Declared),
                error: Some("RESOURCE_PROBE_FAILED"),
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: Some("probe exited 2"),
            },
            Case {
                name: "declared/refresh/destroy-then-run",
                state: ResourceState::Declared,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Refresh {
                    probe: Probe::absent("1"),
                    destroy: None,
                    confirm_destroy: None,
                    run: Some(ok("2")),
                    confirm_run: Some(Probe::present("3")),
                },
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::None,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/run/present",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::present("1"),
                    run: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::None,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/run/absent-recreates-with-gone-notice",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::absent("1"),
                    run: Some(ok("2")),
                    confirm: Some(Probe::present("3")),
                },
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::None,
                instance: true,
                external: false,
                reason: None,
                notices: &["RESOURCE_GONE"],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/run/failed-probe",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::failed_exit("1", 2).unwrap(),
                    run: Some(ok("2")),
                    confirm: None,
                },
                expected: Some(ResourceState::Present),
                error: Some("RESOURCE_PROBE_FAILED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: Some("probe exited 2"),
            },
            Case {
                name: "present/probe/absent",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Probe(Probe::absent("1")),
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &["RESOURCE_GONE"],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/destroy/action",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::present("1"),
                    destroy: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::Destroy,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/destroy/destroy-fail",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::present("1"),
                    destroy: Some(failed("2", "destroy_failed")),
                    confirm: None,
                },
                expected: Some(ResourceState::Orphaned),
                error: Some("DESTROY_FAILED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: Some("destroy_failed"),
                notices: &[],
                finding: None,
                last_error: Some("destroy_failed"),
            },
            Case {
                name: "present/destroy/destroy-ok-awaits-confirm",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::present("1"),
                    destroy: Some(ok("2")),
                    confirm: None,
                },
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::Probe,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/destroy/confirmed-absent-stays-declared",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::present("1"),
                    destroy: Some(ok("2")),
                    confirm: Some(Probe::absent("3")),
                },
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/destroy/teardown-drop",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: true,
                    probe: Probe::present("1"),
                    destroy: Some(ok("2")),
                    confirm: Some(Probe::absent("3")),
                },
                expected: None,
                error: None,
                action: Action::Dropped,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/destroy/undeclared-drop",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: true,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::present("1"),
                    destroy: Some(ok("2")),
                    confirm: Some(Probe::absent("3")),
                },
                expected: None,
                error: None,
                action: Action::Dropped,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "present/destroy/still-present",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::present("1"),
                    destroy: Some(ok("2")),
                    confirm: Some(Probe::present("3")),
                },
                expected: Some(ResourceState::Orphaned),
                error: Some("DESTROY_FAILED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: Some("still_present"),
                notices: &[],
                finding: None,
                last_error: Some("still_present"),
            },
            Case {
                name: "present/destroy/confirm-failed",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: false,
                    probe: Probe::present("1"),
                    destroy: Some(ok("2")),
                    confirm: Some(Probe::failed_exit("3", 2).unwrap()),
                },
                expected: Some(ResourceState::Orphaned),
                error: Some("RESOURCE_PROBE_FAILED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: Some("probe_failed"),
                notices: &[],
                finding: None,
                last_error: Some("probe exited 2"),
            },
            Case {
                name: "present/refresh/destroy-then-run",
                state: ResourceState::Present,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Refresh {
                    probe: Probe::present("1"),
                    destroy: Some(ok("2")),
                    confirm_destroy: Some(Probe::absent("3")),
                    run: Some(ok("4")),
                    confirm_run: Some(Probe::present("5")),
                },
                expected: Some(ResourceState::Present),
                error: None,
                action: Action::None,
                instance: true,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "orphaned/run/refused",
                state: ResourceState::Orphaned,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Run {
                    probe: Probe::present("1"),
                    run: None,
                    confirm: None,
                },
                expected: Some(ResourceState::Orphaned),
                error: Some("RESOURCE_ORPHANED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: Some("old_failure"),
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "orphaned/probe/present-stays-orphaned",
                state: ResourceState::Orphaned,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Probe(Probe::present("1")),
                expected: Some(ResourceState::Orphaned),
                error: None,
                action: Action::None,
                instance: true,
                external: false,
                reason: Some("old_failure"),
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "orphaned/probe/absent-recovers",
                state: ResourceState::Orphaned,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Probe(Probe::absent("1")),
                expected: Some(ResourceState::Declared),
                error: None,
                action: Action::None,
                instance: false,
                external: false,
                reason: None,
                notices: &["RESOURCE_GONE"],
                finding: None,
                last_error: None,
            },
            Case {
                name: "orphaned/probe/failed",
                state: ResourceState::Orphaned,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Probe(Probe::failed_exit("1", 2).unwrap()),
                expected: Some(ResourceState::Orphaned),
                error: None,
                action: Action::None,
                instance: true,
                external: false,
                reason: Some("old_failure"),
                notices: &[],
                finding: Some("RESOURCE_PROBE_FAILED"),
                last_error: Some("probe exited 2"),
            },
            Case {
                name: "orphaned/destroy/retry-fails",
                state: ResourceState::Orphaned,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: true,
                    probe: Probe::present("1"),
                    destroy: Some(failed("2", "retry_failed")),
                    confirm: None,
                },
                expected: Some(ResourceState::Orphaned),
                error: Some("DESTROY_FAILED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: Some("retry_failed"),
                notices: &[],
                finding: None,
                last_error: Some("retry_failed"),
            },
            Case {
                name: "orphaned/destroy/retry-drops",
                state: ResourceState::Orphaned,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Destroy {
                    teardown: true,
                    probe: Probe::present("1"),
                    destroy: Some(ok("2")),
                    confirm: Some(Probe::absent("3")),
                },
                expected: None,
                error: None,
                action: Action::Dropped,
                instance: false,
                external: false,
                reason: None,
                notices: &[],
                finding: None,
                last_error: None,
            },
            Case {
                name: "orphaned/refresh/refused",
                state: ResourceState::Orphaned,
                with_run: true,
                initial_instance: None,
                undeclared: false,
                event: Event::Refresh {
                    probe: Probe::present("1"),
                    destroy: None,
                    confirm_destroy: None,
                    run: None,
                    confirm_run: None,
                },
                expected: Some(ResourceState::Orphaned),
                error: Some("RESOURCE_ORPHANED"),
                action: Action::None,
                instance: true,
                external: false,
                reason: Some("old_failure"),
                notices: &[],
                finding: None,
                last_error: None,
            },
        ];
        for case in cases {
            let mut initial = record(case.state, case.with_run);
            if let Some(has_instance) = case.initial_instance {
                initial.instance = has_instance.then(|| initial.declaration.clone());
            }
            initial.undeclared = case.undeclared;
            let result = step(Some(initial), case.event);
            assert_eq!(
                result.record.as_ref().map(|record| record.state),
                case.expected,
                "{}",
                case.name
            );
            assert_eq!(
                result.error.as_ref().map(|error| error.code.0.as_str()),
                case.error,
                "{}",
                case.name
            );
            assert_eq!(result.action, case.action, "{} action", case.name);
            assert_eq!(
                result
                    .record
                    .as_ref()
                    .is_some_and(|record| record.instance.is_some()),
                case.instance,
                "{} instance",
                case.name
            );
            assert_eq!(
                result.record.as_ref().is_some_and(|record| record.external),
                case.external,
                "{} external",
                case.name
            );
            assert_eq!(
                result
                    .record
                    .as_ref()
                    .and_then(|record| record.reason.as_deref()),
                case.reason,
                "{} reason",
                case.name
            );
            assert_eq!(
                result
                    .notices
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                case.notices,
                "{} notices",
                case.name
            );
            assert_eq!(
                result.finding.as_deref(),
                case.finding,
                "{} finding",
                case.name
            );
            assert_eq!(
                result
                    .record
                    .as_ref()
                    .and_then(|record| record.last_error.as_ref())
                    .map(|error| error.message.as_str()),
                case.last_error,
                "{} last_error",
                case.name
            );
        }
    }

    #[test]
    fn instance_is_frozen_before_run_and_a_failed_probe_never_causes_an_effect() {
        assert_eq!(
            Probe::failed_exit("1", 1).unwrap_err().code.0,
            "PROBE_CONTRACT"
        );
        let freeze = step(
            Some(record(ResourceState::Declared, true)),
            Event::Run {
                probe: Probe::absent("1"),
                run: None,
                confirm: None,
            },
        );
        assert_eq!(freeze.action, Action::Run);
        assert!(freeze.record.unwrap().instance.is_some());

        let failed = step(
            Some(record(ResourceState::Declared, true)),
            Event::Destroy {
                teardown: true,
                probe: Probe::failed_exit("1", 2).unwrap(),
                destroy: Some(ok("2")),
                confirm: None,
            },
        );
        assert_eq!(failed.action, Action::None);
        let record = failed.record.unwrap();
        assert_eq!(record.state, ResourceState::Orphaned);
        assert_eq!(record.reason.as_deref(), Some("probe_failed"));
    }

    #[test]
    fn no_run_resource_stays_declared_with_the_external_notice() {
        let result = step(
            Some(record(ResourceState::Declared, false)),
            Event::Run {
                probe: Probe::absent("1"),
                run: None,
                confirm: None,
            },
        );
        assert_eq!(result.record.unwrap().state, ResourceState::Declared);
        assert_eq!(result.notices, ["RESOURCE_DECLARED_EXTERNAL"]);
    }

    #[test]
    fn default_names_distinguish_tree_repo_and_machine_resources() {
        let tree = snapshot(true).key;
        assert_eq!(default_name_template(&tree), "{{name_short()}}_db");
        let mut repo = tree.clone();
        repo.tied_to = TiedTo::Repo;
        repo.name = None;
        repo.scope = RelDir::new("services/api").unwrap();
        assert_eq!(default_name_template(&repo), "{{label()}}_services_api_db");

        let tree_json = serde_json::to_value(&tree).unwrap();
        assert_eq!(tree_json["name"], "work");
        assert!(tree_json.get("tree_id").is_none());
        let repo_json = serde_json::to_value(&repo).unwrap();
        assert!(repo_json["name"].is_null());

        let mut machine = repo;
        machine.tied_to = TiedTo::Machine;
        machine.label = None;
        assert_eq!(default_name_template(&machine), "machine_services_api_db");
        let machine_json = serde_json::to_value(&machine).unwrap();
        assert!(machine_json["label"].is_null());
        assert!(machine_json["name"].is_null());
    }
}
